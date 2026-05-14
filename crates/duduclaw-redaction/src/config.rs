//! Configuration types for the redaction pipeline.
//!
//! Two layers:
//! - [`RedactionConfig`] — top-level (used as both global `config.toml`
//!   block and agent-level `agent.toml` block).
//! - [`Profile`] — a reusable rule bundle loaded from a profile file
//!   (built-in or `~/.duduclaw/redaction_profiles/custom/`).

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{RedactionError, Result};
use crate::rules::RuleSpec;

/// Default vault TTL in hours (7 days).
pub const DEFAULT_VAULT_TTL_HOURS: i64 = 168;

/// Default purge-after-expire window in days.
pub const DEFAULT_PURGE_AFTER_EXPIRE_DAYS: u32 = 30;

/// Top-level redaction config.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RedactionConfig {
    /// Master switch for this config layer (config.toml or agent.toml).
    pub enabled: bool,

    /// TTL applied to new vault entries.
    pub vault_ttl_hours: i64,

    /// How long to keep expired tokens before permanent purge.
    pub purge_after_expire_days: u32,

    /// `closed` (default — fail-closed) or `open`. MVP only accepts
    /// `closed`; setting `open` raises a config error.
    pub fail_mode: String,

    /// Profile names to combine. Order matters — later overrides
    /// earlier on rule-id collision.
    pub profiles: Vec<String>,

    /// Source-policy overrides.
    pub sources: SourcePolicy,

    /// Tool egress whitelist (default deny). Keys can use `*` wildcard
    /// suffix, e.g. `"odoo.*"`.
    pub tool_egress: HashMap<String, ToolEgressRule>,

    /// Inline rule definitions (override + supplement profile rules).
    pub rules: HashMap<String, RuleSpec>,
}

impl Default for RedactionConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            vault_ttl_hours: DEFAULT_VAULT_TTL_HOURS,
            purge_after_expire_days: DEFAULT_PURGE_AFTER_EXPIRE_DAYS,
            fail_mode: "closed".into(),
            profiles: Vec::new(),
            sources: SourcePolicy::default(),
            tool_egress: HashMap::new(),
            rules: HashMap::new(),
        }
    }
}

impl RedactionConfig {
    /// Validate config invariants. Called after loading.
    pub fn validate(&self) -> Result<()> {
        if self.fail_mode != "closed" {
            return Err(RedactionError::config(format!(
                "fail_mode must be 'closed' in MVP, got '{}'",
                self.fail_mode
            )));
        }
        if self.vault_ttl_hours <= 0 {
            return Err(RedactionError::config(format!(
                "vault_ttl_hours must be positive, got {}",
                self.vault_ttl_hours
            )));
        }
        Ok(())
    }
}

/// Per-source policy ("on" / "off" / "selective" / "inherit").
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SourcePolicy {
    pub user_input: SourceMode,
    pub tool_results: SourceMode,
    pub system_prompt: SourceMode,
    pub sub_agent: SourceMode,
    pub cron_context: SourceMode,
}

impl Default for SourcePolicy {
    fn default() -> Self {
        Self {
            user_input: SourceMode::Off,
            tool_results: SourceMode::On,
            system_prompt: SourceMode::Selective,
            sub_agent: SourceMode::Inherit,
            cron_context: SourceMode::On,
        }
    }
}

/// How a given source class is handled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceMode {
    /// Always redact.
    On,
    /// Never redact (pass through).
    Off,
    /// Only rules with `apply_to_system_prompt = true` fire.
    Selective,
    /// Defer to caller (sub-agent: inherit means trust upstream).
    Inherit,
}

/// Per-tool egress rule.
#[derive(Debug, Clone, Serialize)]
pub struct ToolEgressRule {
    /// `true` = restore token args before executing the tool.
    /// `false` = pass tokens through verbatim (the tool doesn't need real
    ///   values — e.g. an internal log_event tool).
    /// `"deny"` = refuse to execute when args contain tokens.
    #[serde(default)]
    pub restore_args: RestoreArgsMode,

    /// If true, log a separate `egress_allow` audit each time real
    /// values are revealed to this tool.
    #[serde(default)]
    pub audit_reveal: bool,
}

/// How tool args containing tokens are treated.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RestoreArgsMode {
    /// Restore tokens to real values before executing the tool.
    Restore,
    /// Pass tokens through unchanged (tool doesn't need the real value).
    Passthrough,
    /// Refuse to execute the tool when any token-shaped arg is present.
    #[default]
    Deny,
}

// Allow operators to write `restore_args = true` / `false` / `"deny"`
// in toml — accept all three forms.
impl<'de> serde::Deserialize<'de> for ToolEgressRule {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Raw {
            #[serde(default)]
            restore_args: Option<RawRestoreArgs>,
            #[serde(default)]
            audit_reveal: bool,
        }
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum RawRestoreArgs {
            Bool(bool),
            Str(String),
        }
        let raw = Raw::deserialize(deserializer)?;
        let mode = match raw.restore_args {
            None => RestoreArgsMode::default(),
            Some(RawRestoreArgs::Bool(true)) => RestoreArgsMode::Restore,
            Some(RawRestoreArgs::Bool(false)) => RestoreArgsMode::Passthrough,
            Some(RawRestoreArgs::Str(s)) => match s.as_str() {
                "restore" | "true" => RestoreArgsMode::Restore,
                "passthrough" | "false" => RestoreArgsMode::Passthrough,
                "deny" => RestoreArgsMode::Deny,
                other => {
                    return Err(serde::de::Error::custom(format!(
                        "unknown restore_args value: {other}"
                    )));
                }
            },
        };
        Ok(ToolEgressRule {
            restore_args: mode,
            audit_reveal: raw.audit_reveal,
        })
    }
}

/// Reusable rule bundle loaded from a profile file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    pub meta: ProfileMeta,
    #[serde(default)]
    pub rules: HashMap<String, RuleSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileMeta {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub version: String,
}

impl Profile {
    /// Parse a profile from a toml string.
    pub fn from_toml_str(s: &str) -> Result<Self> {
        Ok(toml::from_str(s)?)
    }

    /// Read and parse a profile from disk.
    pub fn from_path<P: AsRef<Path>>(p: P) -> Result<Self> {
        let body = std::fs::read_to_string(p)?;
        Self::from_toml_str(&body)
    }

    /// Merge `other` into self — `other` wins on rule-id collision.
    pub fn merge_in(&mut self, other: Profile) {
        for (id, mut spec) in other.rules {
            spec.id = id.clone();
            self.rules.insert(id, spec);
        }
    }

    /// Flatten the profile's rules into a Vec, ensuring each spec's `id`
    /// matches its map key.
    pub fn into_specs(self) -> Vec<RuleSpec> {
        self.rules
            .into_iter()
            .map(|(id, mut spec)| {
                spec.id = id;
                spec
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_disabled_with_safe_defaults() {
        let c = RedactionConfig::default();
        assert!(!c.enabled);
        assert_eq!(c.vault_ttl_hours, DEFAULT_VAULT_TTL_HOURS);
        assert_eq!(c.fail_mode, "closed");
        assert!(c.validate().is_ok());
    }

    #[test]
    fn fail_mode_open_rejected() {
        let mut c = RedactionConfig::default();
        c.fail_mode = "open".into();
        assert!(c.validate().is_err());
    }

    #[test]
    fn source_policy_defaults_make_sense() {
        let p = SourcePolicy::default();
        assert_eq!(p.user_input, SourceMode::Off);
        assert_eq!(p.tool_results, SourceMode::On);
        assert_eq!(p.system_prompt, SourceMode::Selective);
    }

    #[test]
    fn tool_egress_default_is_deny() {
        let toml_str = r#"
[redaction.tool_egress.web_fetch]
audit_reveal = false
"#;
        #[derive(Deserialize)]
        struct W {
            redaction: RedactionConfig,
        }
        let w: W = toml::from_str(toml_str).unwrap();
        let r = w.redaction.tool_egress.get("web_fetch").unwrap();
        assert_eq!(r.restore_args, RestoreArgsMode::Deny);
    }

    #[test]
    fn tool_egress_accepts_bool_form() {
        let toml_str = r#"
[redaction.tool_egress.send_email]
restore_args = true
audit_reveal = true
"#;
        #[derive(Deserialize)]
        struct W {
            redaction: RedactionConfig,
        }
        let w: W = toml::from_str(toml_str).unwrap();
        let r = w.redaction.tool_egress.get("send_email").unwrap();
        assert_eq!(r.restore_args, RestoreArgsMode::Restore);
        assert!(r.audit_reveal);
    }

    #[test]
    fn tool_egress_accepts_deny_string() {
        let toml_str = r#"
[redaction.tool_egress.web_fetch]
restore_args = "deny"
"#;
        #[derive(Deserialize)]
        struct W {
            redaction: RedactionConfig,
        }
        let w: W = toml::from_str(toml_str).unwrap();
        assert_eq!(
            w.redaction.tool_egress.get("web_fetch").unwrap().restore_args,
            RestoreArgsMode::Deny
        );
    }

    #[test]
    fn profile_parse_and_merge() {
        let a = r#"
[meta]
name = "A"

[rules.email]
type = "regex"
pattern = '''[\w.+-]+@[\w-]+\.[\w.-]+'''
category = "EMAIL"
"#;
        let b = r#"
[meta]
name = "B"

[rules.email]
type = "regex"
pattern = '''[\w]+@[\w]+\.[\w]+'''
category = "EMAIL_STRICT"
priority = 100
"#;
        let mut pa = Profile::from_toml_str(a).unwrap();
        let pb = Profile::from_toml_str(b).unwrap();
        pa.merge_in(pb);
        let spec = pa.rules.get("email").unwrap();
        assert_eq!(spec.category, "EMAIL_STRICT");
        assert_eq!(spec.priority, 100);
    }
}
