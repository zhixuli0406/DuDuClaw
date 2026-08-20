//! `[relay]` configuration — WP-E2 box-side relay client.
//!
//! ## Shape
//!
//! ```toml
//! [relay]
//! enabled = false          # master switch — default OFF (default ON under appliance)
//! url = "https://relay.duduclaw.io"
//! device_name = "客廳主機"  # optional, shown on the cloud's /v1/find page
//! ```
//!
//! ## Loading discipline
//!
//! - **Default off** everywhere except appliance mode: [`RelayConfig::from_home`]
//!   reads `duduclaw_core::is_appliance()` (`DUDUCLAW_APPLIANCE=1`) and, for
//!   any field the operator did NOT explicitly set, resolves it against
//!   that flag — same pattern as `duduclaw_core::appliance::pick_default_bind`
//!   ("only changes what unset resolves to; an explicit value always
//!   wins"). A non-appliance install that never writes `[relay]` is
//!   byte-identical to before this feature existed; an appliance image
//!   that never writes it defaults to `enabled = true` against
//!   [`OFFICIAL_RELAY_URL`].
//! - A missing / malformed `config.toml`, or a missing / malformed
//!   `[relay]` section, resolves to the same appliance-aware defaults —
//!   same defensive convention as `TickConfig::from_home` /
//!   `GoalLoopConfig::from_home`.
//! - `enabled = true` with an empty `url` is refused at spawn time
//!   (`relay_client::spawn_relay_client`), not here — this module only
//!   resolves values, it doesn't decide whether to start anything.

use std::path::Path;

/// Placeholder cloud relay URL. `duduclaw-relay`'s own deployment story
/// (single-instance Cloud Run constraint, persistent-volume decision) is
/// still open — see `crates/duduclaw-relay/README.md`'s "Deployment notes /
/// known risks" section — so this is intentionally a placeholder, not a
/// live endpoint. Appliance images built before the real URL is finalized
/// will simply fail to register/connect (logged, backed off, harmless)
/// until this constant is updated post-deployment.
pub const OFFICIAL_RELAY_URL: &str = "https://relay.duduclaw.io";

#[derive(Debug, Clone, PartialEq)]
pub struct RelayConfig {
    pub enabled: bool,
    pub url: String,
    pub device_name: Option<String>,
}

impl RelayConfig {
    /// Load `[relay]` from `<home>/config.toml`, appliance-aware. Absent /
    /// malformed file or section ⇒ the appliance-aware default (see module
    /// doc).
    pub fn from_home(home_dir: &Path) -> Self {
        let path = home_dir.join("config.toml");
        let content = std::fs::read_to_string(&path).unwrap_or_default();
        Self::from_toml_str(&content, duduclaw_core::is_appliance())
    }

    /// Parse a whole `config.toml` body. Public for tests and for callers
    /// that already hold the file contents. `is_appliance` is injected
    /// (rather than read from the environment here) so the appliance-vs-not
    /// branches are testable without mutating process-global env state —
    /// same separation `duduclaw_core::appliance::pick_default_bind` uses.
    pub fn from_toml_str(content: &str, is_appliance: bool) -> Self {
        let Ok(table) = content.parse::<toml::Table>() else {
            return Self::defaults_for(is_appliance);
        };
        match table.get("relay").and_then(|v| v.as_table()) {
            Some(section) => Self::from_section(section, is_appliance),
            None => Self::defaults_for(is_appliance),
        }
    }

    /// Parse an already-extracted `[relay]` table.
    pub fn from_section(section: &toml::Table, is_appliance: bool) -> Self {
        let enabled = section
            .get("enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(is_appliance);
        let url = section
            .get("url")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| default_url(is_appliance));
        let device_name = section
            .get("device_name")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        Self { enabled, url, device_name }
    }

    fn defaults_for(is_appliance: bool) -> Self {
        Self {
            enabled: is_appliance,
            url: default_url(is_appliance),
            device_name: None,
        }
    }
}

fn default_url(is_appliance: bool) -> String {
    if is_appliance {
        OFFICIAL_RELAY_URL.to_string()
    } else {
        String::new()
    }
}

/// `POST` endpoint for TOFU device registration.
pub fn register_endpoint(base_url: &str) -> String {
    format!("{}/v1/device/register", base_url.trim_end_matches('/'))
}

/// Validate `base_url` and derive the `/v1/device/ws` endpoint for
/// `device_id`.
///
/// Mirrors `tick_config::validate_ws_url`'s loopback carve-out: a loopback
/// host may use plain `http`/`ws` (local dev, and this crate's own
/// integration test against a real `duduclaw-relay` instance bound to
/// `127.0.0.1`); every other host must be `https` and passes through the
/// shared SSRF gate (`web_fetch::validate_url`) before the scheme is
/// swapped to `wss` — the real Cloud Run deployment is always `https`.
pub fn hook_ws_endpoint(base_url: &str, device_id: &str) -> Result<String, String> {
    let parsed = validate_relay_url(base_url)?;
    let ws_scheme = match parsed.scheme() {
        "https" => "wss",
        "http" => "ws",
        other => return Err(format!("unsupported relay url scheme: {other}")),
    };
    let host = parsed
        .host_str()
        .ok_or_else(|| "relay url missing host".to_string())?;
    let port = parsed.port().map(|p| format!(":{p}")).unwrap_or_default();
    Ok(format!("{ws_scheme}://{host}{port}/v1/device/ws?device_id={device_id}"))
}

/// Validate the configured relay base URL (`http`/`https` only; a
/// non-loopback host must be `https` and pass the shared SSRF gate).
pub fn validate_relay_url(base_url: &str) -> Result<reqwest::Url, String> {
    let parsed = reqwest::Url::parse(base_url).map_err(|e| format!("relay url invalid: {e}"))?;
    let scheme = parsed.scheme();
    if scheme != "http" && scheme != "https" {
        return Err(format!(
            "relay url must be http:// or https:// (got {scheme}://)"
        ));
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| "relay url missing host".to_string())?;
    let host_lower = host.to_ascii_lowercase();
    let bare = host_lower.trim_start_matches('[').trim_end_matches(']');
    let is_loopback = bare == "localhost"
        || bare
            .parse::<std::net::IpAddr>()
            .map(|ip| ip.is_loopback())
            .unwrap_or(false);
    if is_loopback {
        return Ok(parsed);
    }

    if scheme != "https" {
        return Err(format!(
            "plaintext http:// is only allowed for loopback hosts \
             (127.0.0.1 / localhost / ::1); use https:// for {host_lower}"
        ));
    }
    crate::web_fetch::validate_url(base_url).map_err(|e| format!("relay url rejected: {e}"))?;
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(body: &str, is_appliance: bool) -> RelayConfig {
        RelayConfig::from_toml_str(body, is_appliance)
    }

    // ── defaults / appliance-awareness ──────────────────────────────

    #[test]
    fn absent_section_defaults_off_when_not_appliance() {
        let cfg = parse("[general]\nlog_level = \"info\"\n", false);
        assert!(!cfg.enabled);
        assert_eq!(cfg.url, "");
        assert_eq!(cfg.device_name, None);
    }

    #[test]
    fn absent_section_defaults_on_with_official_url_when_appliance() {
        let cfg = parse("[general]\nlog_level = \"info\"\n", true);
        assert!(cfg.enabled);
        assert_eq!(cfg.url, OFFICIAL_RELAY_URL);
    }

    #[test]
    fn no_config_file_at_all_behaves_like_absent_section() {
        // Empty string is what `from_home` passes when the file is missing.
        assert_eq!(parse("", false), RelayConfig::defaults_for(false));
        assert_eq!(parse("", true), RelayConfig::defaults_for(true));
    }

    #[test]
    fn malformed_toml_falls_back_to_appliance_aware_defaults() {
        let cfg = parse("this is not = toml [[[", true);
        assert_eq!(cfg, RelayConfig::defaults_for(true));
    }

    #[test]
    fn explicit_enabled_false_wins_even_in_appliance_mode() {
        let cfg = parse("[relay]\nenabled = false\n", true);
        assert!(!cfg.enabled, "explicit operator choice must not be overridden");
        // url still defaults to the official one — only `enabled` was set.
        assert_eq!(cfg.url, OFFICIAL_RELAY_URL);
    }

    #[test]
    fn explicit_enabled_true_wins_when_not_appliance() {
        // `url` is an independent field: turning `enabled` on outside
        // appliance mode does NOT also invent an official-relay default —
        // there is no "the" relay for a non-appliance install. An operator
        // who sets `enabled = true` without `url` gets a config that
        // `spawn_relay_client` refuses to start (logged, fail-closed), which
        // is the intended signal for "you forgot the url", not a silent
        // default endpoint.
        let cfg = parse("[relay]\nenabled = true\n", false);
        assert!(cfg.enabled);
        assert_eq!(cfg.url, "", "no official default outside appliance mode");
    }

    #[test]
    fn explicit_url_overrides_the_default_in_both_modes() {
        let body = "[relay]\nurl = \"https://relay.example.com\"\n";
        assert_eq!(parse(body, false).url, "https://relay.example.com");
        assert_eq!(parse(body, true).url, "https://relay.example.com");
    }

    #[test]
    fn blank_url_is_treated_as_unset() {
        let cfg = parse("[relay]\nurl = \"   \"\n", true);
        assert_eq!(cfg.url, OFFICIAL_RELAY_URL);
    }

    #[test]
    fn device_name_is_trimmed_and_optional() {
        assert_eq!(parse("[relay]\n", false).device_name, None);
        assert_eq!(
            parse("[relay]\ndevice_name = \"  客廳主機  \"\n", false).device_name,
            Some("客廳主機".to_string())
        );
        assert_eq!(parse("[relay]\ndevice_name = \"   \"\n", false).device_name, None);
    }

    // ── endpoint derivation ──────────────────────────────────────────

    #[test]
    fn register_endpoint_strips_trailing_slash() {
        assert_eq!(
            register_endpoint("https://relay.example.com/"),
            "https://relay.example.com/v1/device/register"
        );
        assert_eq!(
            register_endpoint("https://relay.example.com"),
            "https://relay.example.com/v1/device/register"
        );
    }

    #[test]
    fn hook_ws_endpoint_swaps_https_to_wss() {
        let url = hook_ws_endpoint("https://relay.example.com", "box-abc123").unwrap();
        assert_eq!(url, "wss://relay.example.com/v1/device/ws?device_id=box-abc123");
    }

    #[test]
    fn hook_ws_endpoint_preserves_a_non_default_port() {
        let url = hook_ws_endpoint("https://relay.example.com:8443", "box-abc123").unwrap();
        assert_eq!(
            url,
            "wss://relay.example.com:8443/v1/device/ws?device_id=box-abc123"
        );
    }

    #[test]
    fn hook_ws_endpoint_swaps_http_to_ws_on_loopback() {
        let url = hook_ws_endpoint("http://127.0.0.1:8080", "box-abc123").unwrap();
        assert_eq!(url, "ws://127.0.0.1:8080/v1/device/ws?device_id=box-abc123");
    }

    #[test]
    fn hook_ws_endpoint_rejects_plaintext_on_a_public_host() {
        assert!(hook_ws_endpoint("http://relay.example.com", "box-abc123").is_err());
    }

    // ── SSRF / scheme validation ──────────────────────────────────────

    #[test]
    fn validate_relay_url_accepts_a_public_https_host() {
        assert!(validate_relay_url("https://relay.example.com").is_ok());
    }

    #[test]
    fn validate_relay_url_rejects_non_http_schemes() {
        assert!(validate_relay_url("ftp://relay.example.com").is_err());
        assert!(validate_relay_url("ws://relay.example.com").is_err());
    }

    #[test]
    fn validate_relay_url_rejects_ssrf_targets_even_over_https() {
        for url in [
            "https://169.254.169.254/",
            "https://metadata.google.internal/",
            "https://192.168.1.10/",
            "https://10.0.0.5/",
        ] {
            assert!(validate_relay_url(url).is_err(), "{url} must be refused");
        }
    }

    #[test]
    fn validate_relay_url_allows_loopback_variants() {
        for url in [
            "http://127.0.0.1:8080",
            "http://localhost:8080",
            "https://127.0.0.1:8080",
        ] {
            assert!(validate_relay_url(url).is_ok(), "{url} must be allowed");
        }
    }
}
