//! Appliance-profile detection — single authority for "is this gateway
//! process running inside a DuDuClaw appliance image?"
//!
//! DuDuClaw ships a bootable Debian-based appliance image (a headless LAN
//! "duty box" — see the sibling `appliance/` build recipe, tracked in a
//! separate design doc under the public `docs/` tree once published) whose
//! systemd unit sets `DUDUCLAW_APPLIANCE=1` on the gateway process. That env
//! var name is load-bearing (pinned by the image's systemd unit file) and
//! must not change without updating the image recipe in lockstep.
//!
//! Every appliance-aware decision in the gateway/CLI must route through
//! [`is_appliance`] (or the pure [`appliance_flag`] in tests) rather than
//! re-reading the env var directly — one authority, one place to audit.

/// Env var the appliance image's systemd unit sets on the gateway process.
/// Name is pinned — do not rename without updating the appliance image.
pub const APPLIANCE_ENV: &str = "DUDUCLAW_APPLIANCE";

/// Pure decision: is `val` (the raw `DUDUCLAW_APPLIANCE` env var value, if
/// any was read) a truthy appliance-mode flag?
///
/// Exported so callers (and tests) can inject an arbitrary value without
/// mutating real process-global env state — see the `gateway_bind_for_home`
/// tests in `config.rs` for why that matters (env vars are process-global,
/// so any test that flips one for real must serialize against every other
/// test reading it).
///
/// Accepts the conventional truthy spellings (`1`, `true`, `yes`, any
/// ASCII-case variant), trimmed. Anything else — including unset, empty, or
/// a typo like `"on"` — is `false` (fail-closed: the appliance-only surface
/// stays closed unless the flag is unambiguously set).
pub fn appliance_flag(val: Option<&str>) -> bool {
    match val.map(str::trim) {
        Some(v) if v.eq_ignore_ascii_case("1") => true,
        Some(v) if v.eq_ignore_ascii_case("true") => true,
        Some(v) if v.eq_ignore_ascii_case("yes") => true,
        _ => false,
    }
}

/// Is this process running inside a DuDuClaw appliance image?
///
/// Reads the real `DUDUCLAW_APPLIANCE` env var. This is the ONLY place in
/// the codebase that should read that env var directly — every other
/// caller (gateway RPC gates, bind-default resolution) goes through this
/// function.
pub fn is_appliance() -> bool {
    appliance_flag(std::env::var(APPLIANCE_ENV).ok().as_deref())
}

/// Pure: pick the gateway/dashboard bind-address *default* — the fallback
/// used only when neither `DUDUCLAW_BIND` env nor `config.toml [gateway]
/// bind` is set (see `config::resolve_gateway_bind`, which always takes
/// explicit env/config values first). Never overrides an explicit setting;
/// only changes what "nothing set" resolves to.
///
/// An appliance is a headless LAN device — the entire point of plugging it
/// in is reaching the dashboard from another machine on the network — so
/// its unset-default is `0.0.0.0`. Every other install keeps the
/// loopback-first default of `127.0.0.1` (DuDuClaw's existing desktop-safe
/// posture: don't expose the dashboard to the LAN unless asked to).
pub fn pick_default_bind(is_appliance: bool) -> &'static str {
    if is_appliance { "0.0.0.0" } else { "127.0.0.1" }
}

/// Convenience wrapper: `pick_default_bind(is_appliance())`. Used by
/// `config::gateway_bind_for_home` as the `default` argument to
/// `resolve_gateway_bind` — the only integration point, so an env/config
/// value the operator actually set always still wins.
pub fn appliance_default_bind() -> &'static str {
    pick_default_bind(is_appliance())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── appliance_flag (pure — no env access, safe under parallel tests) ──

    #[test]
    fn flag_recognizes_truthy_spellings() {
        assert!(appliance_flag(Some("1")));
        assert!(appliance_flag(Some("true")));
        assert!(appliance_flag(Some("TRUE")));
        assert!(appliance_flag(Some("True")));
        assert!(appliance_flag(Some("yes")));
        assert!(appliance_flag(Some("YES")));
    }

    #[test]
    fn flag_trims_whitespace() {
        assert!(appliance_flag(Some("  1  ")));
        assert!(appliance_flag(Some("\ttrue\n")));
    }

    #[test]
    fn flag_rejects_everything_else() {
        assert!(!appliance_flag(None));
        assert!(!appliance_flag(Some("")));
        assert!(!appliance_flag(Some("0")));
        assert!(!appliance_flag(Some("false")));
        assert!(!appliance_flag(Some("on"))); // close but not a recognized spelling
        assert!(!appliance_flag(Some("2")));
        assert!(!appliance_flag(Some("  ")));
    }

    // ── pick_default_bind (pure — no env access) ───────────────────────

    #[test]
    fn default_bind_is_lan_for_appliance_and_loopback_otherwise() {
        assert_eq!(pick_default_bind(true), "0.0.0.0");
        assert_eq!(pick_default_bind(false), "127.0.0.1");
    }
}
