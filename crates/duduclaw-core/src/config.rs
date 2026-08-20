//! First-run configuration bootstrap.
//!
//! The gateway tolerates a missing `config.toml` at startup (every read of it
//! is defensive — `.ok()` / `unwrap_or_default()`), so the only thing blocking
//! a brand-new install from reaching the dashboard was the CLI's hard-stop.
//! [`write_minimal_config`] writes the smallest valid, parseable config that
//! lets the gateway boot; the rest of first-run setup happens in the browser
//! (the dashboard onboarding flow), not the terminal.

use crate::error::{DuDuClawError, Result};
use std::path::Path;

/// Write a minimal but valid `config.toml` into `home`.
///
/// Only the sections the operator cares about on first boot are written —
/// `[general]` log level + default reply language, and `[gateway]` bind/port.
/// Everything else the gateway reads with `unwrap_or_default`, so this is
/// sufficient to start. The write is atomic (temp file + rename) so a crash
/// mid-write never leaves a half-written config that would fail to parse on
/// the next boot.
///
/// `default_language` is pinned to `zh-TW` — DuDuClaw's primary market is
/// Taiwan — so a brand-new install replies in Traditional Chinese by default
/// (see `duduclaw-gateway::prompt_identity::read_default_language`) without
/// the operator having to touch the dashboard settings page first. This only
/// affects fresh installs: an existing `config.toml` a user already authored
/// is never touched (see the no-op-safe note below), so upgraders keep
/// whatever they had (including no `default_language` at all, which falls
/// back to "follow the user's input language").
///
/// This is a no-op-safe primitive: callers should check for an existing config
/// first; this function always overwrites, so don't call it when a config the
/// user authored already exists.
pub fn write_minimal_config(home: &Path, bind: &str, port: u16) -> Result<()> {
    std::fs::create_dir_all(home)?;

    let content = format!(
        "# DuDuClaw configuration (auto-created on first run).\n\
         # Finish setup in the dashboard — no need to edit this by hand.\n\
         \n\
         [general]\n\
         log_level = \"info\"\n\
         default_language = \"zh-TW\"\n\
         \n\
         [gateway]\n\
         bind = \"{bind}\"\n\
         port = {port}\n"
    );

    let target = home.join("config.toml");
    let tmp = home.join("config.toml.tmp");
    std::fs::write(&tmp, content.as_bytes())?;
    std::fs::rename(&tmp, &target).map_err(|e| {
        // Best-effort cleanup so a failed rename doesn't leave the temp behind.
        let _ = std::fs::remove_file(&tmp);
        DuDuClawError::Io(e)
    })?;
    Ok(())
}

/// Where a resolved `[gateway] bind`/`port` setting came from — surfaced in
/// startup logs so an operator who edited `config.toml` and still sees the
/// old value can immediately tell whether an env var is shadowing it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GatewaySettingSource {
    Env,
    Config,
    Default,
}

impl GatewaySettingSource {
    pub fn label(self) -> &'static str {
        match self {
            GatewaySettingSource::Env => "env",
            GatewaySettingSource::Config => "config.toml",
            GatewaySettingSource::Default => "default",
        }
    }
}

/// Resolve the gateway bind address with priority `DUDUCLAW_BIND` env >
/// `config.toml [gateway] bind` > `default`. Pure function over already-read
/// inputs (no env/fs access) so the priority order is unit-testable.
///
/// # Bug fixed here (2026-08-11 live-verification finding)
///
/// `duduclaw run` used to read `DUDUCLAW_BIND`/`DUDUCLAW_PORT` only,
/// silently ignoring `config.toml [gateway] bind`/`port` on every run after
/// the first (the first run writes them via [`write_minimal_config`], but
/// nothing ever read them back). A user who hand-edited the port in
/// `config.toml` still got a gateway bound to the old value — meanwhile
/// `duduclaw-gateway::deep_link::dashboard_base_url` reads `config.toml
/// [gateway] port` directly to build dashboard links for channel
/// notifications, so every 👉 link pointed at a port nothing was listening
/// on. This lives in `duduclaw-core` (rather than duplicated in
/// `duduclaw-cli` and `duduclaw-gateway::mcp_oauth`) specifically so the
/// CLI's actual bind and every other consumer that needs to predict it
/// (e.g. the MCP OAuth redirect URI) can never disagree by construction.
pub fn resolve_gateway_bind(
    env_val: Option<&str>,
    config_val: Option<&str>,
    default: &str,
) -> (String, GatewaySettingSource) {
    if let Some(v) = env_val.map(str::trim).filter(|s| !s.is_empty()) {
        return (v.to_string(), GatewaySettingSource::Env);
    }
    if let Some(v) = config_val.map(str::trim).filter(|s| !s.is_empty()) {
        return (v.to_string(), GatewaySettingSource::Config);
    }
    (default.to_string(), GatewaySettingSource::Default)
}

/// Resolve the gateway port with priority `DUDUCLAW_PORT` env >
/// `config.toml [gateway] port` > `default`. See [`resolve_gateway_bind`]
/// for the bug this closes. An env var that fails to parse as `u16` is
/// treated as absent (falls through to `config_val`, then `default`) rather
/// than forcing `default` outright, so a malformed env var doesn't shadow a
/// valid `config.toml` setting.
pub fn resolve_gateway_port(
    env_val: Option<&str>,
    config_val: Option<i64>,
    default: u16,
) -> (u16, GatewaySettingSource) {
    if let Some(v) = env_val.and_then(|s| s.trim().parse::<u16>().ok()) {
        return (v, GatewaySettingSource::Env);
    }
    if let Some(v) = config_val.and_then(|v| u16::try_from(v).ok()) {
        return (v, GatewaySettingSource::Config);
    }
    (default, GatewaySettingSource::Default)
}

/// Read the raw (unvalidated) `[gateway] bind`/`port` strings/ints out of
/// `<home>/config.toml`, if it exists and parses. Absent config or absent
/// keys yield `None` for that field — never an error, since callers (e.g.
/// `duduclaw run` on a first boot) must still work with no config file yet.
pub fn read_gateway_raw_settings(home: &Path) -> (Option<String>, Option<i64>) {
    let Ok(content) = std::fs::read_to_string(home.join("config.toml")) else {
        return (None, None);
    };
    let Ok(table) = content.parse::<toml::Table>() else {
        return (None, None);
    };
    let gateway = table.get("gateway").and_then(|v| v.as_table());
    let bind = gateway
        .and_then(|g| g.get("bind"))
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let port = gateway.and_then(|g| g.get("port")).and_then(|v| v.as_integer());
    (bind, port)
}

/// Convenience wrapper: resolve the effective gateway bind address for
/// `home`, reading `DUDUCLAW_BIND` and `<home>/config.toml [gateway] bind`.
/// Default is `127.0.0.1` (loopback-first posture) UNLESS this process is
/// running in appliance mode (`DUDUCLAW_APPLIANCE=1` — see
/// `crate::appliance`), in which case the default is `0.0.0.0`. This only
/// changes what "nothing set" resolves to — an explicit `DUDUCLAW_BIND` env
/// var or `config.toml [gateway] bind` always wins regardless of appliance
/// mode, per `resolve_gateway_bind`'s priority order.
pub fn gateway_bind_for_home(home: &Path) -> (String, GatewaySettingSource) {
    let (config_bind, _) = read_gateway_raw_settings(home);
    let env_bind = std::env::var("DUDUCLAW_BIND").ok();
    resolve_gateway_bind(
        env_bind.as_deref(),
        config_bind.as_deref(),
        crate::appliance::appliance_default_bind(),
    )
}

/// Convenience wrapper: resolve the effective gateway port for `home`,
/// reading `DUDUCLAW_PORT` and `<home>/config.toml [gateway] port`. Default
/// is `18789`, matching the CLI's historical default.
pub fn gateway_port_for_home(home: &Path) -> (u16, GatewaySettingSource) {
    let (_, config_port) = read_gateway_raw_settings(home);
    let env_port = std::env::var("DUDUCLAW_PORT").ok();
    resolve_gateway_port(env_port.as_deref(), config_port, 18789)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_parseable_config_with_gateway_section() {
        let dir = std::env::temp_dir().join(format!("ddc-cfg-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);

        write_minimal_config(&dir, "0.0.0.0", 12345).expect("write should succeed");

        let raw = std::fs::read_to_string(dir.join("config.toml")).expect("config exists");
        let table: toml::Table = raw.parse().expect("config parses as TOML");

        let gateway = table.get("gateway").and_then(|v| v.as_table()).expect("[gateway]");
        assert_eq!(gateway.get("bind").and_then(|v| v.as_str()), Some("0.0.0.0"));
        assert_eq!(gateway.get("port").and_then(|v| v.as_integer()), Some(12345));
        let general = table.get("general").and_then(|v| v.as_table()).expect("[general]");
        assert_eq!(general.get("log_level").and_then(|v| v.as_str()), Some("info"));
        assert_eq!(
            general.get("default_language").and_then(|v| v.as_str()),
            Some("zh-TW")
        );

        // No leftover temp file.
        assert!(!dir.join("config.toml.tmp").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── resolve_gateway_bind / resolve_gateway_port priority order ─────
    //
    // Locks the bug fix: `DUDUCLAW_BIND`/`DUDUCLAW_PORT` env > `config.toml
    // [gateway] bind`/`port` > hardcoded default. Before this fix, the CLI
    // read only the env var, so a hand-edited `config.toml` port was
    // silently ignored on every run after the first.

    #[test]
    fn port_prefers_env_over_config_over_default() {
        let (port, source) = resolve_gateway_port(Some("9000"), Some(9100), 18789);
        assert_eq!(port, 9000);
        assert_eq!(source, GatewaySettingSource::Env);

        let (port, source) = resolve_gateway_port(None, Some(9100), 18789);
        assert_eq!(port, 9100);
        assert_eq!(source, GatewaySettingSource::Config);

        let (port, source) = resolve_gateway_port(None, None, 18789);
        assert_eq!(port, 18789);
        assert_eq!(source, GatewaySettingSource::Default);
    }

    #[test]
    fn port_falls_through_on_unparseable_env_value() {
        // A malformed env var must not shadow a valid config.toml value —
        // it's treated as absent, not as "force default".
        let (port, source) = resolve_gateway_port(Some("not-a-port"), Some(9100), 18789);
        assert_eq!(port, 9100);
        assert_eq!(source, GatewaySettingSource::Config);

        let (port, source) = resolve_gateway_port(Some("not-a-port"), None, 18789);
        assert_eq!(port, 18789);
        assert_eq!(source, GatewaySettingSource::Default);
    }

    #[test]
    fn port_config_value_out_of_u16_range_is_ignored() {
        // A corrupt/out-of-range config value (negative or > 65535) must not
        // panic and must fall through to the default.
        let (port, source) = resolve_gateway_port(None, Some(999_999), 18789);
        assert_eq!(port, 18789);
        assert_eq!(source, GatewaySettingSource::Default);

        let (port, source) = resolve_gateway_port(None, Some(-1), 18789);
        assert_eq!(port, 18789);
        assert_eq!(source, GatewaySettingSource::Default);
    }

    #[test]
    fn bind_prefers_env_over_config_over_default() {
        let (bind, source) = resolve_gateway_bind(Some("0.0.0.0"), Some("10.0.0.5"), "127.0.0.1");
        assert_eq!(bind, "0.0.0.0");
        assert_eq!(source, GatewaySettingSource::Env);

        let (bind, source) = resolve_gateway_bind(None, Some("10.0.0.5"), "127.0.0.1");
        assert_eq!(bind, "10.0.0.5");
        assert_eq!(source, GatewaySettingSource::Config);

        let (bind, source) = resolve_gateway_bind(None, None, "127.0.0.1");
        assert_eq!(bind, "127.0.0.1");
        assert_eq!(source, GatewaySettingSource::Default);
    }

    #[test]
    fn bind_treats_empty_or_whitespace_env_as_absent() {
        // An empty env var (e.g. `DUDUCLAW_BIND=` in a shell profile) must
        // not shadow a real config.toml value.
        let (bind, source) = resolve_gateway_bind(Some("   "), Some("10.0.0.5"), "127.0.0.1");
        assert_eq!(bind, "10.0.0.5");
        assert_eq!(source, GatewaySettingSource::Config);

        let (bind, source) = resolve_gateway_bind(Some(""), None, "127.0.0.1");
        assert_eq!(bind, "127.0.0.1");
        assert_eq!(source, GatewaySettingSource::Default);
    }

    #[test]
    fn backward_compat_neither_env_nor_config_set_behaves_as_before() {
        // The exact pre-fix behavior (both absent) must be unchanged.
        assert_eq!(
            resolve_gateway_port(None, None, 18789),
            (18789, GatewaySettingSource::Default)
        );
        assert_eq!(
            resolve_gateway_bind(None, None, "127.0.0.1"),
            ("127.0.0.1".to_string(), GatewaySettingSource::Default)
        );
    }

    // ── read_gateway_raw_settings ───────────────────────────────────────

    #[test]
    fn read_raw_missing_config_file_yields_none_none() {
        let dir = tempfile::tempdir().unwrap();
        let (bind, port) = read_gateway_raw_settings(dir.path());
        assert_eq!(bind, None);
        assert_eq!(port, None);
    }

    #[test]
    fn read_raw_reads_bind_and_port_from_existing_config() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[gateway]\nbind = \"0.0.0.0\"\nport = 9100\n",
        )
        .unwrap();
        let (bind, port) = read_gateway_raw_settings(dir.path());
        assert_eq!(bind, Some("0.0.0.0".to_string()));
        assert_eq!(port, Some(9100));
    }

    #[test]
    fn read_raw_missing_gateway_table_yields_none_none() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.toml"), "[general]\nlog_level = \"info\"\n").unwrap();
        let (bind, port) = read_gateway_raw_settings(dir.path());
        assert_eq!(bind, None);
        assert_eq!(port, None);
    }

    #[test]
    fn read_raw_malformed_toml_yields_none_none_not_panic() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.toml"), "this is not valid toml {{{").unwrap();
        let (bind, port) = read_gateway_raw_settings(dir.path());
        assert_eq!(bind, None);
        assert_eq!(port, None);
    }

    // ── gateway_bind_for_home / gateway_port_for_home (env + fs) ───────
    //
    // `DUDUCLAW_BIND`/`DUDUCLAW_PORT` are process-global, so these tests
    // serialize on a local lock and restore whatever was there before —
    // same convention as `platform::home_tests::ENV_LOCK`.

    static GATEWAY_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn for_home_honors_config_toml_when_env_absent() {
        let _g = GATEWAY_ENV_LOCK.lock().unwrap();
        let prev_bind = std::env::var("DUDUCLAW_BIND").ok();
        let prev_port = std::env::var("DUDUCLAW_PORT").ok();
        unsafe {
            std::env::remove_var("DUDUCLAW_BIND");
            std::env::remove_var("DUDUCLAW_PORT");
        }

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[gateway]\nbind = \"0.0.0.0\"\nport = 9100\n",
        )
        .unwrap();

        let (bind, bind_source) = gateway_bind_for_home(dir.path());
        assert_eq!(bind, "0.0.0.0");
        assert_eq!(bind_source, GatewaySettingSource::Config);

        let (port, port_source) = gateway_port_for_home(dir.path());
        assert_eq!(port, 9100);
        assert_eq!(port_source, GatewaySettingSource::Config);

        unsafe {
            match prev_bind {
                Some(v) => std::env::set_var("DUDUCLAW_BIND", v),
                None => std::env::remove_var("DUDUCLAW_BIND"),
            }
            match prev_port {
                Some(v) => std::env::set_var("DUDUCLAW_PORT", v),
                None => std::env::remove_var("DUDUCLAW_PORT"),
            }
        }
    }

    #[test]
    fn for_home_env_overrides_config_toml() {
        let _g = GATEWAY_ENV_LOCK.lock().unwrap();
        let prev_bind = std::env::var("DUDUCLAW_BIND").ok();
        let prev_port = std::env::var("DUDUCLAW_PORT").ok();
        unsafe {
            std::env::set_var("DUDUCLAW_BIND", "192.168.1.1");
            std::env::set_var("DUDUCLAW_PORT", "7000");
        }

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[gateway]\nbind = \"0.0.0.0\"\nport = 9100\n",
        )
        .unwrap();

        let (bind, bind_source) = gateway_bind_for_home(dir.path());
        assert_eq!(bind, "192.168.1.1");
        assert_eq!(bind_source, GatewaySettingSource::Env);

        let (port, port_source) = gateway_port_for_home(dir.path());
        assert_eq!(port, 7000);
        assert_eq!(port_source, GatewaySettingSource::Env);

        unsafe {
            match prev_bind {
                Some(v) => std::env::set_var("DUDUCLAW_BIND", v),
                None => std::env::remove_var("DUDUCLAW_BIND"),
            }
            match prev_port {
                Some(v) => std::env::set_var("DUDUCLAW_PORT", v),
                None => std::env::remove_var("DUDUCLAW_PORT"),
            }
        }
    }
}
