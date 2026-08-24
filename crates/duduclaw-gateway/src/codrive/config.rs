//! `[codrive]` config.toml section — CD-1 gateway-side co-drive tunables.
//!
//! Isolation-parsing convention mirrors
//! [`crate::prediction::belief::BeliefConfig::from_home`]: a missing or
//! malformed `config.toml`, or a missing/malformed `[codrive]` section,
//! always degrades to [`CodriveConfig::default`] — never a hard error. Not
//! cached — re-read on every call so an operator edit takes effect on the
//! next `codrive_run` without a gateway restart.

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Socket/token file names `duduclaw-comp` creates under
/// `$XDG_RUNTIME_DIR` (see `duduclaw-comp/src/codrive/mod.rs`). Mirrored
/// here as plain string constants since the gateway cannot depend on that
/// Linux-only, detached-workspace crate — see the module docs on
/// [`super`].
const DEFAULT_SOCKET_FILENAME: &str = "duduclaw-codrive.sock";
const DEFAULT_TOKEN_FILENAME: &str = "duduclaw-codrive.token";

/// Default per-call connect timeout, seconds.
const DEFAULT_CONNECT_TIMEOUT_SECS: u64 = 5;
/// Default ApprovalBroker TTL for a `codrive_action` consequential-step
/// approval request, seconds. Same default window as
/// `approval::DEFAULT_TTL_SECONDS`, kept as an independent literal so this
/// module's config surface stays self-contained.
const DEFAULT_APPROVAL_TTL_SECS: i64 = 3600;
/// Default whole-script wall-clock ceiling, seconds — bounds both the
/// overall run and the frozen-retry poll loop (design §3.1: "「交還」是
/// 明確動作", never an implicit auto-resume; this is the honest-failure
/// backstop if a human never resumes).
const DEFAULT_MAX_SESSION_SECS: u64 = 600;
/// Default for the DESIGN §3.5 plan-approval card — OFF. See
/// [`CodriveConfig::plan_approval`] for why this specific default is a
/// rollout state rather than a safety judgement.
const DEFAULT_PLAN_APPROVAL: bool = false;

/// `config.toml [codrive]`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct CodriveConfig {
    /// Override for the comp injection socket path. `None` ⇒
    /// `$XDG_RUNTIME_DIR/duduclaw-codrive.sock`.
    pub socket_path: Option<String>,
    /// Override for the auth token file path. `None` ⇒ the same directory
    /// as the resolved socket path, filename `duduclaw-codrive.token`.
    pub token_path: Option<String>,
    /// Per-call connect timeout, seconds.
    pub connect_timeout_secs: u64,
    /// ApprovalBroker TTL for a consequential-step approval request,
    /// seconds.
    pub approval_ttl_secs: i64,
    /// Whole-script wall-clock ceiling, seconds — bounds the run and the
    /// frozen-retry poll loop.
    pub max_session_secs: u64,
    /// Plan-approval card (DESIGN §3.5): when `true`, the WHOLE script is
    /// put in front of a human via the existing [`crate::approval::
    /// ApprovalBroker`] BEFORE the comp socket is even opened — one
    /// session-level card on top of (never instead of) the per-step
    /// `codrive_action` gates that already exist.
    ///
    /// **Default `false`, deliberately.** This is a rollout state, not a
    /// judgement that session-level approval is optional: the CD-1/CD-4
    /// live-bridge harness (`live_tests.rs`) drives real VM sessions with
    /// no session-level decider attached, so flipping this default would
    /// make every already-verified live path expire-and-abort at the new
    /// card instead of running. Turning it on is a per-deployment operator
    /// decision today; making it the default is an explicit call for the
    /// next round, once the live harness grows a decider.
    pub plan_approval: bool,
}

impl Default for CodriveConfig {
    fn default() -> Self {
        Self {
            socket_path: None,
            token_path: None,
            connect_timeout_secs: DEFAULT_CONNECT_TIMEOUT_SECS,
            approval_ttl_secs: DEFAULT_APPROVAL_TTL_SECS,
            max_session_secs: DEFAULT_MAX_SESSION_SECS,
            plan_approval: DEFAULT_PLAN_APPROVAL,
        }
    }
}

impl CodriveConfig {
    /// Isolation parsing (same convention as `BeliefConfig::from_home` /
    /// `TaskForwardModelConfig::from_home`): a missing file, unparseable
    /// TOML, missing `[codrive]` table, or a malformed one all degrade to
    /// [`Self::default`] rather than erroring.
    pub fn from_home(home_dir: &Path) -> Self {
        let default = Self::default();
        let path = home_dir.join("config.toml");
        let Ok(raw) = std::fs::read_to_string(&path) else {
            return default;
        };
        let Ok(table) = raw.parse::<toml::Table>() else {
            return default;
        };
        let Some(section) = table.get("codrive") else {
            return default;
        };
        section
            .clone()
            .try_into::<CodriveConfig>()
            .unwrap_or(default)
    }

    /// Resolve the comp injection socket path. `Err` only when no override
    /// is configured AND `$XDG_RUNTIME_DIR` is unset — an honest failure
    /// rather than a silent `/tmp` fallback (world-writable, and the
    /// wrong host on a non-appliance box).
    pub fn resolved_socket_path(&self) -> Result<PathBuf, String> {
        if let Some(p) = &self.socket_path {
            return Ok(PathBuf::from(p));
        }
        let dir = std::env::var_os("XDG_RUNTIME_DIR").ok_or_else(|| {
            "XDG_RUNTIME_DIR is not set and [codrive] socket_path is not configured".to_string()
        })?;
        Ok(PathBuf::from(dir).join(DEFAULT_SOCKET_FILENAME))
    }

    /// Resolve the auth token file path — same directory as the resolved
    /// socket path unless overridden.
    pub fn resolved_token_path(&self) -> Result<PathBuf, String> {
        if let Some(p) = &self.token_path {
            return Ok(PathBuf::from(p));
        }
        let sock = self.resolved_socket_path()?;
        let dir = sock
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        Ok(dir.join(DEFAULT_TOKEN_FILENAME))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir() -> PathBuf {
        let p = std::env::temp_dir().join(format!("codrive-cfg-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn missing_config_toml_yields_default() {
        let home = tempdir();
        assert_eq!(CodriveConfig::from_home(&home), CodriveConfig::default());
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn missing_section_yields_default() {
        let home = tempdir();
        std::fs::write(home.join("config.toml"), "[other]\nfoo = 1\n").unwrap();
        assert_eq!(CodriveConfig::from_home(&home), CodriveConfig::default());
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn malformed_section_yields_default() {
        let home = tempdir();
        std::fs::write(
            home.join("config.toml"),
            "[codrive]\nconnect_timeout_secs = \"nope\"\n",
        )
        .unwrap();
        assert_eq!(CodriveConfig::from_home(&home), CodriveConfig::default());
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn explicit_values_parsed_and_untouched_fields_keep_defaults() {
        let home = tempdir();
        std::fs::write(
            home.join("config.toml"),
            "[codrive]\nsocket_path = \"/tmp/x.sock\"\nmax_session_secs = 30\n",
        )
        .unwrap();
        let cfg = CodriveConfig::from_home(&home);
        assert_eq!(cfg.socket_path.as_deref(), Some("/tmp/x.sock"));
        assert_eq!(cfg.max_session_secs, 30);
        assert_eq!(cfg.approval_ttl_secs, DEFAULT_APPROVAL_TTL_SECS);
        let _ = std::fs::remove_dir_all(&home);
    }

    /// The plan-approval card is OFF unless an operator says otherwise —
    /// pinned so the default can only move as a deliberate, visible edit.
    #[test]
    fn plan_approval_defaults_off() {
        assert!(!CodriveConfig::default().plan_approval);
        let home = tempdir();
        std::fs::write(home.join("config.toml"), "[codrive]\nmax_session_secs = 30\n").unwrap();
        assert!(!CodriveConfig::from_home(&home).plan_approval);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn plan_approval_can_be_turned_on() {
        let home = tempdir();
        std::fs::write(home.join("config.toml"), "[codrive]\nplan_approval = true\n").unwrap();
        assert!(CodriveConfig::from_home(&home).plan_approval);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn resolved_socket_path_uses_override() {
        let cfg = CodriveConfig {
            socket_path: Some("/tmp/x.sock".into()),
            ..CodriveConfig::default()
        };
        assert_eq!(
            cfg.resolved_socket_path().unwrap(),
            PathBuf::from("/tmp/x.sock")
        );
    }

    #[test]
    fn resolved_token_path_defaults_to_socket_dir() {
        let cfg = CodriveConfig {
            socket_path: Some("/run/foo/duduclaw-codrive.sock".into()),
            ..CodriveConfig::default()
        };
        assert_eq!(
            cfg.resolved_token_path().unwrap(),
            PathBuf::from("/run/foo/duduclaw-codrive.token")
        );
    }

    #[test]
    fn resolved_token_path_override_wins() {
        let cfg = CodriveConfig {
            socket_path: Some("/run/foo/duduclaw-codrive.sock".into()),
            token_path: Some("/etc/secret.token".into()),
            ..CodriveConfig::default()
        };
        assert_eq!(
            cfg.resolved_token_path().unwrap(),
            PathBuf::from("/etc/secret.token")
        );
    }
}
