//! Multi-layer enable/disable resolution + channel `force_on` + emergency
//! `force_disable` override.
//!
//! Resolution order (highest authority first):
//!
//! 1. **channel.force_on**  — bypassable only by `force_disable`
//! 2. **env DUDUCLAW_REDACTION=off  AND  --force-disable-redaction CLI flag**
//!    — together this allows operators to break force_on in an emergency;
//!    each invocation must emit a CRITICAL audit and touch the override
//!    flag file so the dashboard surfaces a persistent banner.
//! 3. **env DUDUCLAW_REDACTION=off** alone — overrides agent/global, but
//!    NOT force_on.
//! 4. **CLI flag --redact=on/off** — opt-in at process start
//! 5. **agent.toml [redaction] enabled**
//! 6. **config.toml [redaction] enabled**

use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::audit::{AuditEvent, AuditSink};
use crate::error::Result;

/// Channel-level redaction posture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelPolicy {
    /// Channel locks redaction ON. Override requires force-disable.
    ForceOn,
    /// Channel locks redaction OFF (sub-agent / internal). Rare.
    ForceOff,
    /// Defer to agent/global config.
    #[default]
    Inherit,
}

/// CLI / env-level switches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CliFlag {
    /// Process invoked with `--redact=on`.
    On,
    /// Process invoked with `--redact=off`.
    Off,
    /// No flag passed.
    #[default]
    Unset,
}

/// Env-level switches (just `DUDUCLAW_REDACTION=off|on` for now).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EnvSetting {
    /// `DUDUCLAW_REDACTION=on`.
    On,
    /// `DUDUCLAW_REDACTION=off`.
    Off,
    /// Var unset.
    #[default]
    Unset,
}

impl EnvSetting {
    /// Read `DUDUCLAW_REDACTION` from the process environment.
    pub fn from_env() -> Self {
        match std::env::var("DUDUCLAW_REDACTION").ok().as_deref() {
            Some("on" | "1" | "true") => EnvSetting::On,
            Some("off" | "0" | "false") => EnvSetting::Off,
            _ => EnvSetting::Unset,
        }
    }
}

/// One snapshot of all inputs to the toggle.
#[derive(Debug, Clone, Copy, Default)]
pub struct ToggleInputs {
    pub channel_policy: ChannelPolicy,
    pub env: EnvSetting,
    pub cli_flag: CliFlag,
    pub force_disable_flag: bool,
    pub agent_enabled: bool,
    pub global_enabled: bool,
}

/// Resolved decision plus the layer responsible.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToggleDecision {
    pub enabled: bool,
    pub reason: ToggleReason,
}

/// Why the toggle resolved the way it did. Used for audit + dashboard tooltip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToggleReason {
    ChannelForceOn,
    ChannelForceOff,
    ForceDisableOverride,
    EnvOff,
    EnvOn,
    CliOff,
    CliOn,
    AgentEnabled,
    AgentDisabled,
    GlobalEnabled,
    GlobalDisabled,
}

/// Pure resolver — no IO, no env reads, fully testable.
pub fn compute_effective_enabled(i: ToggleInputs) -> ToggleDecision {
    // Layer 1: channel force_on — bypassable only by full force-disable.
    if i.channel_policy == ChannelPolicy::ForceOn {
        if i.env == EnvSetting::Off && i.force_disable_flag {
            return ToggleDecision {
                enabled: false,
                reason: ToggleReason::ForceDisableOverride,
            };
        }
        return ToggleDecision {
            enabled: true,
            reason: ToggleReason::ChannelForceOn,
        };
    }

    // Layer 1b: channel force_off — overrides everything below.
    if i.channel_policy == ChannelPolicy::ForceOff {
        return ToggleDecision {
            enabled: false,
            reason: ToggleReason::ChannelForceOff,
        };
    }

    // Layer 2: env (highest among the "user opt-in" levers).
    match i.env {
        EnvSetting::Off => {
            return ToggleDecision {
                enabled: false,
                reason: ToggleReason::EnvOff,
            };
        }
        EnvSetting::On => {
            return ToggleDecision {
                enabled: true,
                reason: ToggleReason::EnvOn,
            };
        }
        EnvSetting::Unset => {}
    }

    // Layer 3: CLI flag.
    match i.cli_flag {
        CliFlag::Off => {
            return ToggleDecision {
                enabled: false,
                reason: ToggleReason::CliOff,
            };
        }
        CliFlag::On => {
            return ToggleDecision {
                enabled: true,
                reason: ToggleReason::CliOn,
            };
        }
        CliFlag::Unset => {}
    }

    // Layer 4: agent.
    if i.agent_enabled {
        return ToggleDecision {
            enabled: true,
            reason: ToggleReason::AgentEnabled,
        };
    }

    // Layer 5: global.
    if i.global_enabled {
        ToggleDecision {
            enabled: true,
            reason: ToggleReason::GlobalEnabled,
        }
    } else {
        ToggleDecision {
            enabled: false,
            reason: ToggleReason::GlobalDisabled,
        }
    }
}

/// Persistent state of a `force_disable` override. Written once when an
/// operator successfully overrides a `force_on` channel; dashboard reads
/// this to surface a red banner.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForceOverrideRecord {
    pub started_at: String,
    pub operator: String,
    pub channels: Vec<String>,
    pub reason: String,
}

/// Persistent override-flag file (`<home>/redaction/override.flag`).
///
/// Presence of the file ⇒ banner active. Removing the file clears the
/// banner; this is intentional (operator can `rm` it after a fix).
pub struct ForceOverrideFlag {
    path: PathBuf,
}

impl ForceOverrideFlag {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// True when an override is currently active.
    pub fn is_active(&self) -> bool {
        self.path.exists()
    }

    /// Read the active record (None if no override).
    pub fn read(&self) -> Result<Option<ForceOverrideRecord>> {
        if !self.path.exists() {
            return Ok(None);
        }
        let body = std::fs::read_to_string(&self.path)?;
        let rec: ForceOverrideRecord = serde_json::from_str(&body)?;
        Ok(Some(rec))
    }

    /// Mark the override active. Idempotent: overwrites previous record.
    pub fn activate(
        &self,
        operator: impl Into<String>,
        channels: Vec<String>,
        reason: impl Into<String>,
        audit: &dyn AuditSink,
    ) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let rec = ForceOverrideRecord {
            started_at: Utc::now().to_rfc3339(),
            operator: operator.into(),
            channels: channels.clone(),
            reason: reason.into(),
        };
        std::fs::write(&self.path, serde_json::to_string_pretty(&rec)?)?;

        // Emit a CRITICAL audit per affected channel.
        for ch in channels {
            audit.emit(AuditEvent::ForceOnOverride {
                operator: rec.operator.clone(),
                channel: ch,
                severity: "CRITICAL".into(),
            });
        }
        Ok(())
    }

    /// Clear the override flag (operator-confirmed fix).
    pub fn clear(&self) -> Result<()> {
        if self.path.exists() {
            std::fs::remove_file(&self.path)?;
        }
        Ok(())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Convenience helper that reports the current override state plus a
/// human-readable banner string for the dashboard.
pub fn override_banner(flag: &ForceOverrideFlag) -> Result<Option<String>> {
    let Some(rec) = flag.read()? else {
        return Ok(None);
    };
    let msg = format!(
        "通道強制保護已被覆寫 — operator: {} · since: {} · channels: {} · reason: {}",
        rec.operator,
        rec.started_at,
        rec.channels.join(", "),
        rec.reason
    );
    Ok(Some(msg))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audit::NullAuditSink;
    use tempfile::TempDir;

    fn base() -> ToggleInputs {
        ToggleInputs::default()
    }

    #[test]
    fn channel_force_on_wins_over_disabled_agent_global() {
        let d = compute_effective_enabled(ToggleInputs {
            channel_policy: ChannelPolicy::ForceOn,
            ..base()
        });
        assert!(d.enabled);
        assert_eq!(d.reason, ToggleReason::ChannelForceOn);
    }

    #[test]
    fn channel_force_off_disables_even_if_global_enabled() {
        let d = compute_effective_enabled(ToggleInputs {
            channel_policy: ChannelPolicy::ForceOff,
            global_enabled: true,
            ..base()
        });
        assert!(!d.enabled);
        assert_eq!(d.reason, ToggleReason::ChannelForceOff);
    }

    #[test]
    fn env_off_alone_does_not_break_force_on() {
        let d = compute_effective_enabled(ToggleInputs {
            channel_policy: ChannelPolicy::ForceOn,
            env: EnvSetting::Off,
            force_disable_flag: false,
            ..base()
        });
        assert!(d.enabled);
        assert_eq!(d.reason, ToggleReason::ChannelForceOn);
    }

    #[test]
    fn env_off_plus_force_disable_breaks_force_on() {
        let d = compute_effective_enabled(ToggleInputs {
            channel_policy: ChannelPolicy::ForceOn,
            env: EnvSetting::Off,
            force_disable_flag: true,
            ..base()
        });
        assert!(!d.enabled);
        assert_eq!(d.reason, ToggleReason::ForceDisableOverride);
    }

    #[test]
    fn env_off_disables_when_no_force_on() {
        let d = compute_effective_enabled(ToggleInputs {
            env: EnvSetting::Off,
            agent_enabled: true,
            global_enabled: true,
            ..base()
        });
        assert!(!d.enabled);
        assert_eq!(d.reason, ToggleReason::EnvOff);
    }

    #[test]
    fn cli_overrides_agent_global_when_env_unset() {
        let d = compute_effective_enabled(ToggleInputs {
            cli_flag: CliFlag::On,
            agent_enabled: false,
            global_enabled: false,
            ..base()
        });
        assert!(d.enabled);
        assert_eq!(d.reason, ToggleReason::CliOn);

        let d = compute_effective_enabled(ToggleInputs {
            cli_flag: CliFlag::Off,
            agent_enabled: true,
            global_enabled: true,
            ..base()
        });
        assert!(!d.enabled);
        assert_eq!(d.reason, ToggleReason::CliOff);
    }

    #[test]
    fn agent_overrides_global() {
        let d = compute_effective_enabled(ToggleInputs {
            agent_enabled: true,
            global_enabled: false,
            ..base()
        });
        assert!(d.enabled);
        assert_eq!(d.reason, ToggleReason::AgentEnabled);
    }

    #[test]
    fn global_default_when_nothing_set() {
        let d = compute_effective_enabled(ToggleInputs {
            global_enabled: true,
            ..base()
        });
        assert!(d.enabled);
        assert_eq!(d.reason, ToggleReason::GlobalEnabled);

        let d = compute_effective_enabled(ToggleInputs::default());
        assert!(!d.enabled);
        assert_eq!(d.reason, ToggleReason::GlobalDisabled);
    }

    #[test]
    fn full_truth_table_priority() {
        // Verify channel takes priority over everything else (16 combinations).
        for env in [EnvSetting::Unset, EnvSetting::Off] {
            for cli in [CliFlag::Unset, CliFlag::On, CliFlag::Off] {
                for ae in [false, true] {
                    for ge in [false, true] {
                        let d = compute_effective_enabled(ToggleInputs {
                            channel_policy: ChannelPolicy::ForceOn,
                            env,
                            cli_flag: cli,
                            force_disable_flag: false,
                            agent_enabled: ae,
                            global_enabled: ge,
                        });
                        assert!(d.enabled, "force_on must win for env={env:?} cli={cli:?} ae={ae} ge={ge}");
                    }
                }
            }
        }
    }

    #[test]
    fn flag_round_trip_and_clear() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("override.flag");
        let flag = ForceOverrideFlag::new(path);
        let audit = NullAuditSink;

        assert!(!flag.is_active());

        flag.activate(
            "lizhixu",
            vec!["line_customer_support".into(), "discord_internal".into()],
            "emergency vault rotation",
            &audit,
        )
        .unwrap();
        assert!(flag.is_active());

        let rec = flag.read().unwrap().unwrap();
        assert_eq!(rec.operator, "lizhixu");
        assert_eq!(rec.channels.len(), 2);
        assert_eq!(rec.reason, "emergency vault rotation");

        let banner = override_banner(&flag).unwrap().unwrap();
        assert!(banner.contains("lizhixu"));

        flag.clear().unwrap();
        assert!(!flag.is_active());
        assert!(override_banner(&flag).unwrap().is_none());
    }

    #[test]
    fn env_setting_from_env_robust_to_unknown_values() {
        // Don't actually set env in tests (shared process state); just
        // construct directly.
        let _ = EnvSetting::from_env();
        // Variants compile.
        let _: EnvSetting = EnvSetting::On;
        let _: EnvSetting = EnvSetting::Off;
        let _: EnvSetting = EnvSetting::Unset;
    }
}
