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

/// Byte cap for a poison `reason` — a TOML error can carry a long excerpt and
/// this string ends up in a dashboard banner. CJK-safe (never slices mid-char).
pub const POISON_REASON_MAX_BYTES: usize = 500;

/// What `config.toml` says about redaction at gateway boot.
///
/// The three outcomes are deliberately distinct: before 2026-09 a deserialize
/// failure collapsed into `None` and took the same silent path as "not
/// configured", so a typo'd `[redaction]` block ran the gateway unredacted with
/// nothing louder than a DEBUG line. See DESIGN-redaction-field-rules-2026-09 §12.
#[derive(Debug, Clone)]
pub enum BootOutcome {
    /// No `config.toml`, no `[redaction]` section, or `enabled = false`.
    /// Nothing is built — identical to the pre-2026-09 behaviour.
    Disabled,
    /// `[redaction]` parsed and `enabled = true`.
    Enabled(Box<RedactionConfig>),
    /// Redaction could not be resolved from config. The gateway still boots,
    /// but enters the poison state (loud log + Activity Feed + dashboard
    /// banner) instead of silently running without protection.
    Poisoned(String),
}

/// Classify the `[redaction]` boot outcome from raw `config.toml` text.
///
/// `None` = the file does not exist (fresh install) ⇒ [`BootOutcome::Disabled`].
///
/// A whole-file TOML syntax error poisons rather than disabling: with the
/// document unparseable we cannot prove redaction was *not* requested, and
/// "cannot tell" must never resolve to "run unprotected" (fail closed).
pub fn classify_redaction_boot(raw_config_text: Option<&str>) -> BootOutcome {
    let Some(raw) = raw_config_text else {
        return BootOutcome::Disabled;
    };
    let table: toml::Table = match toml::from_str(raw) {
        Ok(t) => t,
        Err(e) => {
            return BootOutcome::Poisoned(poison_reason(format!("config.toml 解析失敗：{e}")));
        }
    };
    let Some(section) = table.get("redaction") else {
        return BootOutcome::Disabled;
    };
    let cfg: RedactionConfig = match section.clone().try_into() {
        Ok(c) => c,
        Err(e) => {
            return BootOutcome::Poisoned(poison_reason(format!("[redaction] 設定解析失敗：{e}")));
        }
    };
    if cfg.enabled {
        BootOutcome::Enabled(Box::new(cfg))
    } else {
        BootOutcome::Disabled
    }
}

/// Normalise a poison reason for storage/display: single line, byte-capped
/// (CJK-safe — never a raw byte slice, per the project's coding conventions).
pub fn poison_reason(raw: impl AsRef<str>) -> String {
    let flat = raw.as_ref().replace(['\n', '\r'], " ");
    let trimmed = flat.trim();
    duduclaw_core::truncate_bytes(trimmed, POISON_REASON_MAX_BYTES).to_string()
}

/// Best-effort Activity Feed row for a redaction lifecycle event
/// (`redaction_init_failed` / `redaction_recovered`).
///
/// Mirrors `auth_outage::post_activity` — telemetry, never control flow: if
/// the task store cannot be opened or the append fails we log at debug and
/// move on, because an alarm bell must not be the reason the gateway fails.
pub async fn post_redaction_activity(home_dir: &Path, event_type: &str, summary: &str) {
    let store = match crate::task_store::TaskStore::open(home_dir) {
        Ok(s) => s,
        Err(e) => {
            tracing::debug!(error = %e, "redaction activity: task store unavailable (non-fatal)");
            return;
        }
    };
    let row = crate::task_store::ActivityRow {
        id: uuid::Uuid::new_v4().to_string(),
        event_type: event_type.to_string(),
        agent_id: String::new(),
        task_id: None,
        summary: summary.to_string(),
        timestamp: chrono::Utc::now().to_rfc3339(),
        metadata: None,
    };
    if let Err(e) = store.append_activity(&row).await {
        tracing::debug!(error = %e, "redaction activity: append failed (non-fatal)");
    }
}

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

    // ── §12 boot classification ──────────────────────────────

    #[test]
    fn missing_config_is_disabled_not_poisoned() {
        assert!(matches!(classify_redaction_boot(None), BootOutcome::Disabled));
    }

    #[test]
    fn config_without_redaction_section_is_disabled() {
        let raw = "[general]\ndefault_agent = \"kiki\"\n";
        assert!(matches!(
            classify_redaction_boot(Some(raw)),
            BootOutcome::Disabled
        ));
    }

    #[test]
    fn redaction_disabled_is_disabled() {
        let raw = "[redaction]\nenabled = false\nprofiles = [\"general\"]\n";
        assert!(matches!(
            classify_redaction_boot(Some(raw)),
            BootOutcome::Disabled
        ));
    }

    #[test]
    fn redaction_enabled_yields_the_parsed_config() {
        let raw = "[redaction]\nenabled = true\nprofiles = [\"general\"]\n";
        match classify_redaction_boot(Some(raw)) {
            BootOutcome::Enabled(cfg) => {
                assert!(cfg.enabled);
                assert_eq!(cfg.profiles, vec!["general".to_string()]);
            }
            other => panic!("expected Enabled, got {other:?}"),
        }
    }

    #[test]
    fn malformed_redaction_section_poisons_instead_of_disabling() {
        // `vault_ttl_hours` must be an integer — the old `.ok()` path turned
        // this into `None` and ran the gateway unredacted in silence.
        let raw = "[redaction]\nenabled = true\nvault_ttl_hours = \"forever\"\n";
        match classify_redaction_boot(Some(raw)) {
            BootOutcome::Poisoned(reason) => assert!(
                reason.contains("[redaction]"),
                "reason should name the section: {reason}"
            ),
            other => panic!("expected Poisoned, got {other:?}"),
        }
    }

    #[test]
    fn unknown_rule_type_poisons() {
        let raw = concat!(
            "[redaction]\nenabled = true\n",
            "[redaction.rules.oops]\ntype = \"not_a_kind\"\ncategory = \"X\"\n"
        );
        assert!(matches!(
            classify_redaction_boot(Some(raw)),
            BootOutcome::Poisoned(_)
        ));
    }

    #[test]
    fn whole_file_syntax_error_poisons() {
        let raw = "[redaction\nenabled = true\n";
        match classify_redaction_boot(Some(raw)) {
            BootOutcome::Poisoned(reason) => {
                assert!(reason.contains("config.toml"), "{reason}")
            }
            other => panic!("expected Poisoned, got {other:?}"),
        }
    }

    #[test]
    fn poison_reason_is_single_line_and_byte_capped() {
        let long = format!("壞掉了\n{}", "壞".repeat(1000));
        let r = poison_reason(&long);
        assert!(!r.contains('\n'));
        assert!(r.len() <= POISON_REASON_MAX_BYTES);
        // CJK-safe: the cap walked back to a char boundary.
        assert!(std::str::from_utf8(r.as_bytes()).is_ok());
    }

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
