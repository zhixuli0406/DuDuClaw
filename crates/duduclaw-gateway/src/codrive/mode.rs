//! A2 driving-mode state machine — the gateway-side mirror of the closed
//! enums `duduclaw-comp` reports on its co-drive socket.
//!
//! Wire contract (A2 §1): comp derives `mode` as a pure function of
//! `(session_active, terminated, frozen)` and keeps no second copy that
//! could drift. This module deliberately does NOT re-derive anything — it
//! only parses what comp reports, so there is exactly one state machine in
//! the system and it lives on the compositor side.
//!
//! Shadow is deliberately not a fourth mode (A2 §1): while an agent works
//! in a shadow output, the SHARED desktop's driving seat is still the
//! human's, so comp reports `mode = human` alongside `shadow = true`. The
//! two travel side by side in [`super::client::CodriveAck`]; never fold one
//! into the other.
//!
//! Two hard rules, both because the gateway and comp are separately
//! deployed binaries whose versions will skew on a real appliance:
//!
//! 1. **An absent field is not an error.** Every new status field on
//!    [`super::client::CodriveAck`] is `#[serde(default)]`; a comp that
//!    predates A2 answers the old three-field shape and must keep parsing.
//! 2. **An unrecognized token is not an error either — and must never be
//!    silently rounded down to [`CodriveDrivingMode::Human`].** "Human"
//!    means "no co-drive session, the agent has zero driving authority";
//!    guessing that when the truth is "comp knows a mode this gateway does
//!    not" would be the least safe reading available. Unknown tokens land
//!    verbatim (length-capped) in [`CodriveDrivingMode::Unknown`] /
//!    [`CodriveHandoverReason::Unknown`] and are logged at `warn`.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Cap on a verbatim-preserved unknown token. A skewed — or hostile —
/// comp must not be able to push an unbounded string into a run report,
/// an audit line, or a log record through this door. CJK-safe (codepoint
/// count, not bytes) per this project's coding convention 1.
const MAX_UNKNOWN_TOKEN_CHARS: usize = 64;

/// Which seat is driving THIS shared desktop (A2 §1). Closed set; anything
/// else is [`Self::Unknown`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodriveDrivingMode {
    /// No co-drive session (or it was emergency-stopped). The agent has
    /// zero driving authority over the shared desktop.
    Human,
    /// An authenticated session exists and the agent seat is not frozen —
    /// the agent is driving, the human is watching.
    CoDrive,
    /// A session exists but the agent seat is frozen — the human has the
    /// wheel. A pause, not a termination.
    Handover,
    /// A token this gateway build does not know. Preserved verbatim
    /// (capped at [`MAX_UNKNOWN_TOKEN_CHARS`]) so the skew is visible in
    /// the report instead of being laundered into a wrong-but-plausible
    /// state.
    Unknown(String),
}

impl CodriveDrivingMode {
    /// Parse one wire token. Never fails — see the module doc's rule 2.
    pub fn from_wire(raw: &str) -> Self {
        match raw {
            "human" => Self::Human,
            "codrive" => Self::CoDrive,
            "handover" => Self::Handover,
            other => {
                let kept = duduclaw_core::truncate_chars(other, MAX_UNKNOWN_TOKEN_CHARS);
                tracing::warn!(
                    token = %kept,
                    "codrive: comp reported an unknown driving mode — recorded verbatim, NOT treated as human"
                );
                Self::Unknown(kept)
            }
        }
    }

    /// The wire token for this mode. `Unknown` round-trips verbatim.
    pub fn as_wire(&self) -> &str {
        match self {
            Self::Human => "human",
            Self::CoDrive => "codrive",
            Self::Handover => "handover",
            Self::Unknown(s) => s.as_str(),
        }
    }

    /// True iff this is a token this build understands — the honest
    /// version-skew signal for a caller that wants to say so out loud.
    pub fn is_known(&self) -> bool {
        !matches!(self, Self::Unknown(_))
    }
}

impl std::fmt::Display for CodriveDrivingMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_wire())
    }
}

/// Why the seat is in [`CodriveDrivingMode::Handover`] (A2 §2). Only
/// meaningful in that mode; comp serializes `null` otherwise, which
/// deserializes to `None` (not to a variant).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodriveHandoverReason {
    /// The human touched the shared desktop — the always-wins path
    /// (DESIGN §6 red line 3).
    HumanInput,
    /// The agent handed the wheel over on purpose (`take_over`).
    AgentTakeOver,
    /// Watch mode saw nobody present and auto-paused.
    WatchIdle,
    /// The human pressed the shell's "take the wheel" control.
    ShellTakeWheel,
    /// A token this gateway build does not know — same doctrine as
    /// [`CodriveDrivingMode::Unknown`].
    Unknown(String),
}

impl CodriveHandoverReason {
    /// Parse one wire token. Never fails.
    pub fn from_wire(raw: &str) -> Self {
        match raw {
            "human_input" => Self::HumanInput,
            "agent_take_over" => Self::AgentTakeOver,
            "watch_idle" => Self::WatchIdle,
            "shell_take_wheel" => Self::ShellTakeWheel,
            other => {
                let kept = duduclaw_core::truncate_chars(other, MAX_UNKNOWN_TOKEN_CHARS);
                tracing::warn!(
                    token = %kept,
                    "codrive: comp reported an unknown handover reason — recorded verbatim"
                );
                Self::Unknown(kept)
            }
        }
    }

    /// The wire token for this reason. `Unknown` round-trips verbatim.
    pub fn as_wire(&self) -> &str {
        match self {
            Self::HumanInput => "human_input",
            Self::AgentTakeOver => "agent_take_over",
            Self::WatchIdle => "watch_idle",
            Self::ShellTakeWheel => "shell_take_wheel",
            Self::Unknown(s) => s.as_str(),
        }
    }

    /// True iff this is a token this build understands.
    pub fn is_known(&self) -> bool {
        !matches!(self, Self::Unknown(_))
    }
}

impl std::fmt::Display for CodriveHandoverReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_wire())
    }
}

// ── serde: hand-written on purpose ──────────────────────────────────────
// A derived enum would make an unknown token a HARD DECODE ERROR, which on
// this wire means "a comp one version ahead of this gateway bricks every
// status reply". These impls route every token through `from_wire`, so the
// unknown case is a recorded value rather than a failure. Serializing
// yields the plain wire token (never an externally-tagged object), so a run
// report reads `"mode":"codrive"` exactly like the wire does.

impl Serialize for CodriveDrivingMode {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_wire())
    }
}

impl<'de> Deserialize<'de> for CodriveDrivingMode {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Ok(Self::from_wire(&raw))
    }
}

impl Serialize for CodriveHandoverReason {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_wire())
    }
}

impl<'de> Deserialize<'de> for CodriveHandoverReason {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Ok(Self::from_wire(&raw))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_known_mode_token_round_trips() {
        for (token, want) in [
            ("human", CodriveDrivingMode::Human),
            ("codrive", CodriveDrivingMode::CoDrive),
            ("handover", CodriveDrivingMode::Handover),
        ] {
            let parsed = CodriveDrivingMode::from_wire(token);
            assert_eq!(parsed, want);
            assert_eq!(parsed.as_wire(), token);
            assert!(parsed.is_known());
            assert_eq!(
                serde_json::to_value(&parsed).unwrap(),
                serde_json::json!(token),
                "mode must serialize as the bare wire token"
            );
        }
    }

    #[test]
    fn every_known_reason_token_round_trips() {
        for (token, want) in [
            ("human_input", CodriveHandoverReason::HumanInput),
            ("agent_take_over", CodriveHandoverReason::AgentTakeOver),
            ("watch_idle", CodriveHandoverReason::WatchIdle),
            ("shell_take_wheel", CodriveHandoverReason::ShellTakeWheel),
        ] {
            let parsed = CodriveHandoverReason::from_wire(token);
            assert_eq!(parsed, want);
            assert_eq!(parsed.as_wire(), token);
            assert!(parsed.is_known());
            assert_eq!(
                serde_json::to_value(&parsed).unwrap(),
                serde_json::json!(token)
            );
        }
    }

    /// The whole reason these impls are hand-written: a comp one version
    /// ahead of this gateway must not turn every status reply into a decode
    /// error.
    #[test]
    fn unknown_mode_token_is_preserved_not_an_error_and_not_human() {
        let parsed: CodriveDrivingMode =
            serde_json::from_value(serde_json::json!("teleop")).unwrap();
        assert_eq!(parsed, CodriveDrivingMode::Unknown("teleop".to_string()));
        assert!(!parsed.is_known());
        assert_ne!(
            parsed,
            CodriveDrivingMode::Human,
            "an unknown mode must never be laundered into `human`"
        );
        assert_eq!(
            parsed.as_wire(),
            "teleop",
            "unknown tokens round-trip verbatim"
        );
    }

    #[test]
    fn unknown_reason_token_is_preserved_not_an_error() {
        let parsed: CodriveHandoverReason =
            serde_json::from_value(serde_json::json!("cosmic_ray")).unwrap();
        assert_eq!(
            parsed,
            CodriveHandoverReason::Unknown("cosmic_ray".to_string())
        );
        assert!(!parsed.is_known());
    }

    /// A hostile/skewed comp cannot push an unbounded token into a report,
    /// a log line, or an audit row through this door. CJK-safe (codepoints).
    #[test]
    fn unknown_token_is_length_capped_without_splitting_a_codepoint() {
        let long = "駕".repeat(500);
        let parsed = CodriveDrivingMode::from_wire(&long);
        let CodriveDrivingMode::Unknown(kept) = parsed else {
            panic!("must classify as Unknown");
        };
        assert_eq!(kept.chars().count(), MAX_UNKNOWN_TOKEN_CHARS);
        assert!(kept.chars().all(|c| c == '駕'));
    }

    /// An empty token is still an unknown token, not a default.
    #[test]
    fn empty_token_is_unknown_not_human() {
        assert_eq!(
            CodriveDrivingMode::from_wire(""),
            CodriveDrivingMode::Unknown(String::new())
        );
    }

    /// Casing/whitespace variants are NOT normalized — comp's tokens are an
    /// exact closed set (coding convention 2: no fuzzy matching on a
    /// routing/security-adjacent decision).
    #[test]
    fn token_matching_is_exact() {
        assert!(!CodriveDrivingMode::from_wire("CoDrive").is_known());
        assert!(!CodriveDrivingMode::from_wire(" codrive").is_known());
        assert!(!CodriveHandoverReason::from_wire("Human_Input").is_known());
    }

    #[test]
    fn display_matches_wire_token() {
        assert_eq!(CodriveDrivingMode::CoDrive.to_string(), "codrive");
        assert_eq!(CodriveHandoverReason::WatchIdle.to_string(), "watch_idle");
    }
}
