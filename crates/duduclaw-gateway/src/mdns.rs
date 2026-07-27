//! mDNS / DNS-SD service advertising for LAN gateway discovery (WP-GW).
//!
//! An enterprise gateway lives on a shared server; employees run the desktop
//! app and need to *find* it on the local network before logging in. This
//! module advertises the gateway as `_duduclaw._tcp.local.` so the desktop
//! shell's `gateway_discover` command can browse and list reachable gateways.
//!
//! Design (`commercial/docs/design-gateway-picker.md` §2 / §2.5):
//! - service type `_duduclaw._tcp.local.`, instance = the gateway's display name
//! - TXT records: `version`, `name`, `tls=0/1`
//! - `config.toml [server] mdns_advertise` (default **false** since §2.5 — an
//!   opt-in "office gateway" broadcasts, everyone else stays quiet to prevent
//!   gateway sprawl on the LAN); `[server] tls` marks an HTTPS front
//! - env override `DUDUCLAW_MDNS_ADVERTISE` (`0`/`1`) takes precedence over
//!   config — desktop-app sidecars inject `=0` so an employee laptop never
//!   turns into a discoverable gateway
//! - register on startup, unregister on graceful shutdown
//! - advertising is strictly best-effort: any failure is logged as a warning
//!   and never blocks the gateway from serving.

use mdns_sd::{ServiceDaemon, ServiceInfo};

/// The DNS-SD service type all DuDuClaw gateways advertise under.
pub const SERVICE_TYPE: &str = "_duduclaw._tcp.local.";

/// Env var overriding `[server] mdns_advertise`. Takes precedence over config so
/// desktop-app sidecars can inject `DUDUCLAW_MDNS_ADVERTISE=0` and never appear
/// on the LAN, regardless of what the user's `config.toml` says.
pub const MDNS_ADVERTISE_ENV: &str = "DUDUCLAW_MDNS_ADVERTISE";

/// Resolve the effective advertise decision: an env override (if it parses to a
/// bool) wins; otherwise the config value stands. Unset / garbage env falls
/// through to config — the env can only *decide*, never accidentally flip on a
/// malformed value.
pub fn resolve_advertise(config_advertise: bool, env_val: Option<&str>) -> bool {
    match env_val.and_then(parse_bool) {
        Some(v) => v,
        None => config_advertise,
    }
}

/// Resolved advertising settings, parsed from `config.toml` (fail-safe).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MdnsConfig {
    /// Whether to advertise at all (`[server] mdns_advertise`, default false).
    pub advertise: bool,
    /// Human-facing instance name (`[general] name`, default = hostname).
    pub name: String,
    /// Whether the reachable endpoint is fronted by HTTPS (`[server] tls`).
    /// Purely informational in the TXT record — the gateway itself binds HTTP;
    /// TLS is terminated by an operator reverse proxy.
    pub tls: bool,
}

impl MdnsConfig {
    /// Parse advertising settings from raw `config.toml` text. Fail-safe: any
    /// missing / malformed value falls back to a sensible default, and the
    /// whole thing never panics. `default_name` (typically the OS hostname) is
    /// used when `[general] name` is absent or empty.
    ///
    /// A minimal section-aware line scanner — deliberately dependency-free so
    /// the parse stays trivially testable and mirrors `lifecycle`-style config
    /// readers elsewhere. `[server] mdns_advertise` defaults to **false** (opt
    /// in, not opt out — only a deliberately-configured office gateway
    /// broadcasts; see design §2.5 "防 gateway 蔓延").
    pub fn from_toml_str(text: &str, default_name: &str) -> Self {
        let mut advertise: Option<bool> = None;
        let mut tls: Option<bool> = None;
        let mut name: Option<String> = None;
        let mut section = String::new();

        for raw in text.lines() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some(sec) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
                section = sec.trim().to_ascii_lowercase();
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let key = key.trim().to_ascii_lowercase();
            // Strip an inline comment and surrounding whitespace/quotes.
            let val = value
                .split('#')
                .next()
                .unwrap_or("")
                .trim()
                .trim_matches('"');
            match (section.as_str(), key.as_str()) {
                ("server", "mdns_advertise") => advertise = parse_bool(val),
                ("server", "tls") => tls = parse_bool(val),
                ("general", "name") if !val.is_empty() => name = Some(val.to_string()),
                _ => {}
            }
        }

        let name = name
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| default_name.to_string());
        MdnsConfig {
            advertise: advertise.unwrap_or(false),
            name: if name.trim().is_empty() {
                "DuDuClaw".to_string()
            } else {
                name
            },
            tls: tls.unwrap_or(false),
        }
    }
}

/// Parse a TOML-ish boolean literal (`true`/`false`/`1`/`0`, case-insensitive).
fn parse_bool(v: &str) -> Option<bool> {
    match v.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Some(true),
        "false" | "0" | "no" | "off" => Some(false),
        _ => None,
    }
}

/// Build the ordered TXT key/value pairs for the advertised service. Kept pure
/// and separate so the wire format is unit-tested independently of the daemon.
pub fn build_txt_properties(version: &str, name: &str, tls: bool) -> Vec<(String, String)> {
    vec![
        ("version".to_string(), version.to_string()),
        ("name".to_string(), name.to_string()),
        ("tls".to_string(), if tls { "1" } else { "0" }.to_string()),
    ]
}

/// A gateway candidate reconstructed from a browsed/advertised record. Mirrors
/// the desktop-side `GatewayRecord` (the desktop crate is detached and cannot
/// depend on the gateway, so this is the canonical mapping spec both sides
/// agree on — tested here, re-implemented identically in `src-tauri`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayAdvert {
    pub name: String,
    pub host: String,
    pub port: u16,
    pub version: String,
    pub tls: bool,
    pub url: String,
}

/// Reconstruct a [`GatewayAdvert`] from the pieces a browser resolves: a TXT
/// lookup closure, the mDNS hostname, the port, and the first reachable IPv4.
///
/// - `url` is built from the IPv4 for reliable connectivity (an mDNS `.local.`
///   hostname is not always resolvable off the advertising host), with the
///   scheme chosen from the `tls` TXT flag.
/// - `host` shows the human-friendly `.local` hostname (trailing dot stripped),
///   falling back to the IP when the hostname is empty.
/// - Missing TXT values fail safe: `tls` absent → false, `name` absent → host,
///   `version` absent → empty string.
pub fn parse_gateway_advert<F>(
    txt: F,
    hostname: &str,
    port: u16,
    ipv4: Option<&str>,
) -> GatewayAdvert
where
    F: Fn(&str) -> Option<String>,
{
    let tls = txt("tls")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    let version = txt("version").unwrap_or_default();
    let host_display = {
        // `office-server.local.` → `office-server` (strip trailing dot + the
        // conventional `.local` mDNS suffix for a clean display label).
        let h = hostname.trim_end_matches('.');
        let h = h.strip_suffix(".local").unwrap_or(h);
        if h.is_empty() {
            ipv4.unwrap_or("").to_string()
        } else {
            h.to_string()
        }
    };
    // Connectivity target: prefer the concrete IPv4; fall back to the hostname.
    let target = ipv4
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(|| host_display.clone());
    let scheme = if tls { "https" } else { "http" };
    let url = format!("{scheme}://{target}:{port}/");
    let name = txt("name")
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| host_display.clone());
    GatewayAdvert {
        name,
        host: host_display,
        port,
        version,
        tls,
        url,
    }
}

/// A live mDNS advertisement. Holds the daemon + the registered fullname so the
/// gateway can cleanly unregister on graceful shutdown.
pub struct MdnsAdvertiser {
    daemon: ServiceDaemon,
    fullname: String,
}

impl MdnsAdvertiser {
    /// Register the gateway on the LAN. Best-effort: returns `Err` on any mDNS
    /// failure so the caller can `warn!` and carry on serving.
    ///
    /// `hostname` is the bare OS hostname (no `.local.`); the port is the
    /// gateway's listen port. Interface addresses are auto-detected
    /// (`enable_addr_auto`) so this works across macOS / Linux / Windows and
    /// multi-homed hosts without the caller enumerating NICs.
    pub fn start(
        cfg: &MdnsConfig,
        hostname: &str,
        port: u16,
        version: &str,
    ) -> Result<Self, String> {
        let daemon = ServiceDaemon::new().map_err(|e| format!("mdns daemon init: {e}"))?;

        let host_fqdn = format!("{}.local.", hostname.trim_end_matches('.'));
        let props = build_txt_properties(version, &cfg.name, cfg.tls);

        // Empty IP + enable_addr_auto → the daemon fills in (and keeps current)
        // every interface address, so we never hard-code a NIC.
        let info = ServiceInfo::new(SERVICE_TYPE, &cfg.name, &host_fqdn, "", port, &props[..])
            .map_err(|e| format!("mdns service info: {e}"))?
            .enable_addr_auto();

        let fullname = info.get_fullname().to_string();
        daemon
            .register(info)
            .map_err(|e| format!("mdns register: {e}"))?;
        Ok(MdnsAdvertiser { daemon, fullname })
    }

    /// The registered service fullname (e.g. `Office GW._duduclaw._tcp.local.`).
    pub fn fullname(&self) -> &str {
        &self.fullname
    }

    /// Unregister + shut the daemon down. Best-effort — errors are logged by the
    /// caller. Consumes `self` so it is only run once (on graceful shutdown).
    pub fn stop(self) {
        if let Err(e) = self.daemon.unregister(&self.fullname) {
            tracing::warn!("mdns unregister failed: {e}");
        }
        // Give the goodbye packet a beat to leave before tearing the daemon down.
        std::thread::sleep(std::time::Duration::from_millis(120));
        if let Err(e) = self.daemon.shutdown() {
            tracing::warn!("mdns daemon shutdown failed: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_defaults_to_advertise_false_with_hostname() {
        // Empty config → advertise OFF by default (§2.5 opt-in), name = provided
        // hostname default, tls off.
        let cfg = MdnsConfig::from_toml_str("", "office-server");
        assert_eq!(
            cfg,
            MdnsConfig {
                advertise: false,
                name: "office-server".to_string(),
                tls: false,
            }
        );
    }

    #[test]
    fn config_opt_in_true_enables_advertising() {
        let cfg = MdnsConfig::from_toml_str("[server]\nmdns_advertise = true\n", "h");
        assert!(cfg.advertise);
    }

    #[test]
    fn config_reads_server_and_general_sections() {
        let toml = "\
[general]
name = \"Acme Gateway\"

[server]
mdns_advertise = false
tls = true
";
        let cfg = MdnsConfig::from_toml_str(toml, "fallback");
        assert!(!cfg.advertise);
        assert!(cfg.tls);
        assert_eq!(cfg.name, "Acme Gateway");
    }

    #[test]
    fn config_ignores_name_outside_general_and_tolerates_comments() {
        // `name` under [gateway] must NOT be picked up; only [general].
        let toml = "\
[gateway]
name = \"wrong\"

[server]
mdns_advertise = 1  # inline comment
";
        let cfg = MdnsConfig::from_toml_str(toml, "host1");
        assert!(cfg.advertise);
        assert_eq!(cfg.name, "host1"); // fell back to default, not "wrong"
    }

    #[test]
    fn config_bool_variants_and_bad_values_fail_safe() {
        assert!(MdnsConfig::from_toml_str("[server]\nmdns_advertise = yes\n", "h").advertise);
        assert!(!MdnsConfig::from_toml_str("[server]\nmdns_advertise = off\n", "h").advertise);
        // Garbage value → default false (fail-safe: stay quiet unless explicitly
        // told to broadcast).
        assert!(!MdnsConfig::from_toml_str("[server]\nmdns_advertise = maybe\n", "h").advertise);
    }

    #[test]
    fn env_override_takes_precedence_over_config() {
        // Env forces OFF even when config opted in.
        assert!(!resolve_advertise(true, Some("0")));
        assert!(!resolve_advertise(true, Some("false")));
        // Env forces ON even when config (default) is off.
        assert!(resolve_advertise(false, Some("1")));
        assert!(resolve_advertise(true, Some("on")));
        // Unset or garbage env → config value stands (env can't flip on a typo).
        assert!(!resolve_advertise(false, None));
        assert!(resolve_advertise(true, None));
        assert!(resolve_advertise(true, Some("maybe")));
        assert!(!resolve_advertise(false, Some("garbage")));
    }

    #[test]
    fn txt_properties_have_expected_order_and_values() {
        let props = build_txt_properties("1.45.0", "Acme Gateway", true);
        assert_eq!(props[0], ("version".to_string(), "1.45.0".to_string()));
        assert_eq!(props[1], ("name".to_string(), "Acme Gateway".to_string()));
        assert_eq!(props[2], ("tls".to_string(), "1".to_string()));
        let props = build_txt_properties("1.45.0", "x", false);
        assert_eq!(props[2].1, "0");
    }

    /// mDNS TXT parse unit test (mocked resolver) — the desktop `gateway_discover`
    /// mapping spec. Verifies scheme selection, host display, and URL assembly.
    #[test]
    fn parse_advert_builds_http_url_from_ipv4() {
        let txt = |k: &str| match k {
            "version" => Some("1.45.0".to_string()),
            "name" => Some("Office GW".to_string()),
            "tls" => Some("0".to_string()),
            _ => None,
        };
        let rec = parse_gateway_advert(txt, "office-server.local.", 18789, Some("192.168.1.20"));
        assert_eq!(rec.name, "Office GW");
        assert_eq!(rec.host, "office-server"); // trailing `.local.` stripped
        assert_eq!(rec.port, 18789);
        assert_eq!(rec.version, "1.45.0");
        assert!(!rec.tls);
        assert_eq!(rec.url, "http://192.168.1.20:18789/");
    }

    #[test]
    fn parse_advert_tls_flag_selects_https() {
        let txt = |k: &str| match k {
            "tls" => Some("1".to_string()),
            _ => None,
        };
        let rec = parse_gateway_advert(txt, "gw.local.", 443, Some("10.0.0.5"));
        assert!(rec.tls);
        assert_eq!(rec.url, "https://10.0.0.5:443/");
        // name absent → falls back to host display.
        assert_eq!(rec.name, "gw");
    }

    #[test]
    fn parse_advert_falls_back_to_hostname_without_ipv4() {
        let txt = |_: &str| None;
        let rec = parse_gateway_advert(txt, "gw.local.", 18789, None);
        // No IPv4 → target is the hostname; no TXT → http, version empty.
        assert_eq!(rec.url, "http://gw:18789/");
        assert_eq!(rec.version, "");
        assert!(!rec.tls);
    }
}
