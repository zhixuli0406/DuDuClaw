//! Gateway-side integration shim for `duduclaw-redaction`.
//!
//! This module is a *minimal-touch* adoption layer for RFC-23. The
//! redaction crate is self-contained; this file exposes a single optional
//! `Arc<RedactionManager>` that downstream call sites can consult.
//!
//! ## Adoption recipe
//!
//! 1. Build a `RedactionManager` at server start time from `config.toml` +
//!    per-agent `agent.toml [redaction]` blocks. See [`build_manager_from_home`].
//! 2. Store it as `Option<Arc<RedactionManager>>` on `GatewayConfig` (or
//!    wherever the gateway keeps shared state).
//! 3. At each integration point (tool result emission, LLM call, channel
//!    reply, tool dispatch) branch on `Some(manager)`:
//!    `manager.pipeline(agent, session)?.redact(text, source)`,
//!    `pipeline.restore(reply, caller, target)`,
//!    `manager.decide_tool_call(name, args, agent, session)`.
//! 4. Resolve toggle at the channel layer via
//!    [`compute_effective_for_channel`].
//!
//! `None` ⇒ no redaction (existing behaviour preserved).

use std::path::Path;
use std::sync::Arc;

use duduclaw_redaction::{
    ChannelPolicy, CliFlag, EnvSetting, ManagerPaths, RedactionConfig, RedactionError,
    RedactionManager, ToggleDecision, ToggleInputs, compute_effective_enabled,
};

/// Read the CLI `--redact=on/off` flag persisted in `DUDUCLAW_REDACT_CLI_FLAG`.
/// `entry_point()` writes this env var before dispatching to subcommands.
pub fn cli_flag_from_env() -> CliFlag {
    match std::env::var("DUDUCLAW_REDACT_CLI_FLAG").ok().as_deref() {
        Some("on" | "true" | "1") => CliFlag::On,
        Some("off" | "false" | "0") => CliFlag::Off,
        _ => CliFlag::Unset,
    }
}

/// True if the persistent force-disable flag file is currently active.
/// `<home>/redaction/override.flag`. Cheap stat check; safe to call per-call.
pub fn force_disable_active(home: &Path) -> bool {
    home.join("redaction").join("override.flag").exists()
}

/// Convenience: build a `RedactionManager` rooted at the DuDuClaw home
/// directory, using the supplied config.
///
/// Returns `Ok(None)` if `config.enabled == false` AT the global layer —
/// callers can still construct a manager and resolve per-agent / channel
/// toggles afterwards, but for many deployments "global disabled" is
/// equivalent to "don't run".
pub fn build_manager_from_home(
    home: &Path,
    config: RedactionConfig,
) -> Result<Arc<RedactionManager>, RedactionError> {
    let paths = ManagerPaths::under_home(home);
    let manager = RedactionManager::open(config, paths)?;
    Ok(Arc::new(manager))
}

/// Resolve the effective enable/disable for a specific channel call.
///
/// `manager.config_enabled()` is the global-layer setting; the agent's
/// per-agent toggle is passed separately (gateway already loads agent.toml).
pub fn compute_effective_for_channel(
    manager: Option<&Arc<RedactionManager>>,
    channel_policy: ChannelPolicy,
    cli_flag: CliFlag,
    agent_enabled: bool,
    force_disable_flag: bool,
) -> ToggleDecision {
    let (global, _has_mgr) = match manager {
        Some(m) => (m.config_enabled(), true),
        None => (false, false),
    };
    compute_effective_enabled(ToggleInputs {
        channel_policy,
        env: EnvSetting::from_env(),
        cli_flag,
        force_disable_flag,
        agent_enabled,
        global_enabled: global,
    })
}

/// Quick check: should this `(channel, agent)` pair attempt redaction?
///
/// Equivalent to `compute_effective_for_channel(...).enabled` plus a
/// requirement that `manager.is_some()`.
pub fn is_redaction_active(
    manager: Option<&Arc<RedactionManager>>,
    channel_policy: ChannelPolicy,
    cli_flag: CliFlag,
    agent_enabled: bool,
    force_disable_flag: bool,
) -> bool {
    if manager.is_none() {
        return false;
    }
    compute_effective_for_channel(
        manager,
        channel_policy,
        cli_flag,
        agent_enabled,
        force_disable_flag,
    )
    .enabled
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn none_manager_always_disabled() {
        assert!(!is_redaction_active(
            None,
            ChannelPolicy::ForceOn,
            CliFlag::On,
            true,
            false
        ));
    }

    #[test]
    fn manager_present_respects_channel_force_on() {
        let tmp = TempDir::new().unwrap();
        let mut cfg = RedactionConfig::default();
        cfg.enabled = false; // global off
        cfg.profiles = vec!["general".into()];
        let m = build_manager_from_home(tmp.path(), cfg).unwrap();
        // channel force_on overrides global-off.
        assert!(is_redaction_active(
            Some(&m),
            ChannelPolicy::ForceOn,
            CliFlag::Unset,
            false,
            false,
        ));
    }

    #[test]
    fn agent_toggle_is_respected_when_no_channel_policy() {
        let tmp = TempDir::new().unwrap();
        let mut cfg = RedactionConfig::default();
        cfg.enabled = false;
        cfg.profiles = vec!["general".into()];
        let m = build_manager_from_home(tmp.path(), cfg).unwrap();
        assert!(is_redaction_active(
            Some(&m),
            ChannelPolicy::Inherit,
            CliFlag::Unset,
            true,
            false,
        ));
        assert!(!is_redaction_active(
            Some(&m),
            ChannelPolicy::Inherit,
            CliFlag::Unset,
            false,
            false,
        ));
    }
}
