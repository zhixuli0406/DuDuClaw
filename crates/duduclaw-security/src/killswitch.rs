//! KILLSWITCH.toml configuration parser.
//!
//! Declarative safety boundaries for AI agent operation, inspired by
//! the [KILLSWITCH.md](https://killswitch.md/) open standard (2026-03).
//!
//! The config defines triggers, forbidden actions, escalation protocols,
//! safety words, and audit requirements.

use std::path::Path;

use serde::{Deserialize, Serialize};
use tracing::warn;

// ── Config structs ─────────────────────────────────────────────

/// Top-level killswitch configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
#[derive(Default)]
pub struct KillswitchConfig {
    pub triggers: TriggersConfig,
    pub circuit_breaker: CircuitBreakerConfig,
    pub failsafe: FailsafeConfig,
    pub safety_words: SafetyWordsConfig,
    pub defensive_prompt: DefensivePromptConfig,
    pub audit: AuditConfig,
}

/// Trigger thresholds that cause escalation.
///
/// **Note**: These values are declarative boundaries read by the gateway's
/// rate limiter and cost telemetry modules at startup. They are not
/// enforced by the killswitch module itself — enforcement is the
/// responsibility of the consuming module (e.g., `RateLimiter` reads
/// `max_replies_per_minute`, `CostTelemetry` reads `cost_limit_usd`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TriggersConfig {
    /// Max replies per minute per scope before rate-limiting kicks in.
    /// Read by `RateLimiter` at initialization.
    pub max_replies_per_minute: u32,
    /// Max consecutive errors before failsafe escalation.
    pub max_consecutive_errors: u32,
    /// Error rate (0.0–1.0) threshold for failsafe escalation.
    pub error_rate_threshold: f64,
    /// Daily API cost limit in USD. Read by `CostTelemetry`.
    pub cost_limit_usd: f64,
}

/// Circuit breaker behavioral anomaly detection thresholds.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CircuitBreakerConfig {
    /// Sliding window duration for frequency detection (seconds).
    pub frequency_window_secs: u64,
    /// Max replies within the frequency window before tripping.
    pub frequency_max_replies: u32,
    /// Content similarity threshold (0.0–1.0) for repetition detection.
    /// Uses simple hash-based equality when >= 1.0, byte-level Jaccard otherwise.
    pub similarity_threshold: f64,
    /// Token count multiplier over rolling average to detect explosion.
    pub token_explosion_multiplier: f64,
    /// Cooldown duration after tripping (seconds) before trying half-open.
    pub cooldown_secs: u64,
    /// Number of requests allowed through in half-open state for probing.
    pub half_open_allow_count: u32,
}

/// Failsafe graceful degradation configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct FailsafeConfig {
    /// Seconds before L1 (Degraded) auto-recovers to L0. 0 = manual only.
    pub l1_auto_recover_secs: u64,
    /// Seconds before L2 (Restricted) auto-recovers to L0. 0 = manual only.
    pub l2_auto_recover_secs: u64,
    /// Seconds before L3 (Muted) auto-recovers to L0. 0 = manual only.
    pub l3_auto_recover_secs: u64,
    /// Canned reply when in L2 (Restricted).
    pub default_restricted_reply: String,
    /// Canned reply when in L4 (Halted).
    pub default_halted_reply: String,
}

/// Configurable safety words (multi-language).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SafetyWordsConfig {
    /// Words that stop the current agent/scope.
    pub stop: Vec<String>,
    /// Words that stop ALL agents globally.
    pub stop_all: Vec<String>,
    /// Words that resume a stopped agent/scope.
    pub resume: Vec<String>,
    /// Words that query the current safety status.
    pub status: Vec<String>,
}

impl SafetyWordsConfig {
    /// Whether any configured word does NOT start with `!`.
    ///
    /// Pre-computed for the hot-path fast-reject in `safety_word::check()`.
    /// When false (all words start with `!`), non-`!` messages can be
    /// rejected immediately without iterating the word list.
    pub fn has_non_bang_prefix(&self) -> bool {
        self.stop.iter()
            .chain(&self.stop_all)
            .chain(&self.resume)
            .chain(&self.status)
            .any(|w| !w.starts_with('!'))
    }
}

/// Defensive prompt injection for bot-loop prevention.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DefensivePromptConfig {
    /// Whether defensive prompts are enabled.
    pub enabled: bool,
    /// Languages to include in the defensive prompt.
    pub languages: Vec<String>,
}

/// Audit logging configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AuditConfig {
    /// Whether audit logging is enabled.
    pub enabled: bool,
    /// Path to the audit JSONL file (supports `~` expansion).
    pub path: String,
}

// ── Defaults ───────────────────────────────────────────────────


impl Default for TriggersConfig {
    fn default() -> Self {
        Self {
            max_replies_per_minute: 10,
            max_consecutive_errors: 5,
            error_rate_threshold: 0.3,
            cost_limit_usd: 50.0,
        }
    }
}

impl Default for CircuitBreakerConfig {
    fn default() -> Self {
        Self {
            frequency_window_secs: 10,
            frequency_max_replies: 5,
            similarity_threshold: 0.95,
            token_explosion_multiplier: 3.0,
            cooldown_secs: 60,
            half_open_allow_count: 2,
        }
    }
}

impl Default for FailsafeConfig {
    fn default() -> Self {
        Self {
            l1_auto_recover_secs: 300,
            l2_auto_recover_secs: 600,
            l3_auto_recover_secs: 0,
            default_restricted_reply: "系統暫時限制回覆，請稍後再試。".to_string(),
            default_halted_reply: "服務已暫停。如需恢復請聯繫管理員。".to_string(),
        }
    }
}

impl Default for SafetyWordsConfig {
    fn default() -> Self {
        Self {
            stop: vec![
                "!STOP".to_string(),
                "!停止".to_string(),
                "!緊急停止".to_string(),
            ],
            stop_all: vec![
                "!STOP ALL".to_string(),
                "!全部停止".to_string(),
            ],
            resume: vec![
                "!RESUME".to_string(),
                "!恢復".to_string(),
                "!繼續".to_string(),
            ],
            status: vec![
                "!STATUS".to_string(),
                "!狀態".to_string(),
            ],
        }
    }
}

impl Default for DefensivePromptConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            languages: vec!["en".to_string(), "zh-TW".to_string()],
        }
    }
}

impl Default for AuditConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            path: "~/.duduclaw/killswitch_audit.jsonl".to_string(),
        }
    }
}

// ── Loading ────────────────────────────────────────────────────

impl KillswitchConfig {
    /// Load configuration from a TOML file. Returns defaults if file is missing.
    pub fn load(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(content) => match toml::from_str::<KillswitchConfig>(&content) {
                Ok(config) => {
                    if let Err(e) = config.validate() {
                        warn!("KILLSWITCH.toml validation warning: {e}");
                    }
                    config
                }
                Err(e) => {
                    warn!("Failed to parse KILLSWITCH.toml: {e}, using defaults");
                    Self::default()
                }
            },
            Err(_) => Self::default(),
        }
    }

    /// Validate configuration values for sanity.
    pub fn validate(&self) -> Result<(), String> {
        let mut warnings = Vec::new();

        if self.triggers.error_rate_threshold < 0.0 || self.triggers.error_rate_threshold > 1.0 {
            warnings.push("error_rate_threshold must be 0.0–1.0");
        }
        if self.circuit_breaker.similarity_threshold < 0.0
            || self.circuit_breaker.similarity_threshold > 1.0
        {
            warnings.push("similarity_threshold must be 0.0–1.0");
        }
        if self.circuit_breaker.token_explosion_multiplier < 1.0 {
            warnings.push("token_explosion_multiplier must be >= 1.0");
        }
        if self.safety_words.stop.is_empty() {
            warnings.push("safety_words.stop should have at least one entry");
        }

        if warnings.is_empty() {
            Ok(())
        } else {
            Err(warnings.join("; "))
        }
    }

    /// Resolve the audit log path, expanding `~/.duduclaw/` to `home_dir`.
    ///
    /// `home_dir` is expected to be `~/.duduclaw/` (the DuDuClaw home directory).
    pub fn resolved_audit_path(&self, home_dir: &Path) -> std::path::PathBuf {
        if self.audit.path.starts_with("~/.duduclaw/") {
            home_dir.join(&self.audit.path["~/.duduclaw/".len()..])
        } else if self.audit.path.starts_with("~/") {
            // Generic ~ expansion: treat home_dir's parent as the user home
            if let Some(parent) = home_dir.parent() {
                parent.join(&self.audit.path[2..])
            } else {
                home_dir.join(&self.audit.path[2..])
            }
        } else {
            std::path::PathBuf::from(&self.audit.path)
        }
    }
}

// ── Tests ──────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_valid() {
        let config = KillswitchConfig::default();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn parse_minimal_toml() {
        let toml_str = r#"
[triggers]
max_replies_per_minute = 20

[safety_words]
stop = ["!HALT", "!停"]
"#;
        let config: KillswitchConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.triggers.max_replies_per_minute, 20);
        assert_eq!(config.safety_words.stop, vec!["!HALT", "!停"]);
        // Other fields should be defaults
        assert_eq!(config.triggers.max_consecutive_errors, 5);
        assert!(config.validate().is_ok());
    }

    #[test]
    fn parse_full_toml() {
        let toml_str = r#"
[triggers]
max_replies_per_minute = 15
max_consecutive_errors = 3
error_rate_threshold = 0.2
cost_limit_usd = 100.0

[circuit_breaker]
frequency_window_secs = 5
frequency_max_replies = 3
similarity_threshold = 0.9
token_explosion_multiplier = 2.5
cooldown_secs = 120
half_open_allow_count = 1

[failsafe]
l1_auto_recover_secs = 60
l2_auto_recover_secs = 120
l3_auto_recover_secs = 0
default_restricted_reply = "Limited mode."
default_halted_reply = "Service paused."

[safety_words]
stop = ["!STOP"]
stop_all = ["!STOP ALL"]
resume = ["!GO"]
status = ["!CHECK"]

[defensive_prompt]
enabled = false
languages = ["en"]

[audit]
enabled = true
path = "/var/log/killswitch.jsonl"
"#;
        let config: KillswitchConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.circuit_breaker.cooldown_secs, 120);
        assert!(!config.defensive_prompt.enabled);
        assert_eq!(config.audit.path, "/var/log/killswitch.jsonl");
    }

    #[test]
    fn validation_catches_bad_values() {
        let mut config = KillswitchConfig::default();
        config.triggers.error_rate_threshold = 1.5;
        assert!(config.validate().is_err());

        let mut config = KillswitchConfig::default();
        config.circuit_breaker.token_explosion_multiplier = 0.5;
        assert!(config.validate().is_err());
    }

    #[test]
    fn load_missing_file_returns_default() {
        let config = KillswitchConfig::load(Path::new("/nonexistent/KILLSWITCH.toml"));
        assert_eq!(config.triggers.max_replies_per_minute, 10);
    }

    #[test]
    fn audit_path_resolution() {
        let config = KillswitchConfig::default();
        let home = Path::new("/home/user/.duduclaw");
        let resolved = config.resolved_audit_path(home);
        assert_eq!(resolved, std::path::PathBuf::from("/home/user/.duduclaw/killswitch_audit.jsonl"));
    }
}
