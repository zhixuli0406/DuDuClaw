//! Personal-edition passwordless local session (WP-F1, design doc
//! `commercial/docs/ux-redesign-2026-08/09-edition-split-features.md` §2,
//! decision D3 = plan A "loopback 自動發 session").
//!
//! A single-owner (Personal) install running on its default `127.0.0.1` bind
//! has exactly one human, sitting at the machine. Making that human type a
//! password into their own laptop buys nothing: anything able to reach the
//! endpoint can already read `~/.duduclaw/jwt_secret` (§2.4.4). So the
//! dashboard asks the gateway for a session and the gateway issues a **normal
//! JWT pair for the real `admin@local` user** — not a synthetic context. That
//! choice is the whole point of plan A: every downstream authorization
//! decision (203 `require_admin!` sites, the WS handshake, audit attribution)
//! keeps working byte-identically, because only the *way the token is
//! obtained* is new.
//!
//! This module holds the **decision** half — pure functions with no I/O beyond
//! reading `config.toml` — so each rejection branch is unit-testable without
//! standing up an HTTP server. The handler (`server.rs::handle_local_session`)
//! is the I/O half.
//!
//! ## The gate
//!
//! Six conditions, ALL required. Any miss returns one uniform 403 — the caller
//! never learns *which* one failed, mirroring `handle_first_run_status`'s
//! refusal to advertise `claimable` off-loopback. An off-loopback prober must
//! not be able to fingerprint the edition or the switch state.
//!
//! 1. the switch is on ([`auto_login_enabled`]);
//! 2. the edition is Personal;
//! 3. the TCP peer is loopback (`ConnectInfo`, **never** a header — a header is
//!    attacker-controlled, the peer address is not);
//! 4. no proxy header is present ([`PROXY_HEADERS`]) — see below;
//! 5. the `Origin`, if any, passes the existing loopback allowlist;
//! 6. the request carries `X-DuDuClaw-Local: 1`.
//!
//! ## Why 4 and 6 exist
//!
//! **4 — reverse proxy (§2.4.2, the single red cell of the threat matrix).**
//! With nginx/Caddy on the same box, `ConnectInfo` reports *nginx's* loopback
//! address while the actual client may be anywhere on the internet: the
//! fail-safe silently inverts into fail-open. The defence is deliberately
//! *existence-based, not parsing-based* — we refuse outright when any
//! `X-Forwarded-For`-class header is present. Parsing "the leftmost IP" would
//! be worse than useless: that value is forgeable, and trusting it would throw
//! away the peer address, the one input an attacker cannot control.
//!
//! **6 — malicious website in the operator's own browser (§2.4.3).** A page on
//! `evil.example` can POST to `http://127.0.0.1:18789/…`. CORS stops it from
//! *reading* the token, but a "simple request" is still *sent*, which would be
//! enough to trigger the implicit claim. Requiring a custom header makes the
//! request non-simple, so the browser must send a CORS preflight first — and
//! the preflight gets no allow-origin answer from this gateway, so it never
//! reaches the handler at all.

use axum::http::HeaderMap;
use std::path::Path;

/// The single-owner account a local session is issued for. Same row
/// `ensure_default_admin` bootstraps and `claim_default_admin` claims.
pub(crate) const LOCAL_ADMIN_EMAIL: &str = "admin@local";

/// Header the dashboard must send. Its *value* matters less than its
/// existence (a custom header is what forces the preflight), but we pin `1`
/// so the contract is explicit and greppable.
pub(crate) const LOCAL_MARKER_HEADER: &str = "x-duduclaw-local";
/// Required value of [`LOCAL_MARKER_HEADER`].
pub(crate) const LOCAL_MARKER_VALUE: &str = "1";

/// Headers whose mere presence proves the request crossed a proxy, making the
/// TCP peer address meaningless as an identity signal.
///
/// The first three are the design mandate (§2.4.2). The rest are the same
/// class of hop-injected client-identity headers (CDN / cloud LB variants);
/// including them only ever makes the gate stricter, and no direct
/// browser-to-loopback request carries any of them.
pub(crate) const PROXY_HEADERS: &[&str] = &[
    "x-forwarded-for",
    "x-real-ip",
    "forwarded",
    "x-forwarded-host",
    "x-forwarded-proto",
    "cf-connecting-ip",
    "true-client-ip",
];

/// Environment override for the config switch. `0`/`false`/`off`/`no` disables;
/// `1`/`true`/`on`/`yes`/`auto` enables. Anything else is ignored (falls
/// through to `config.toml`), so a typo cannot silently disable the dashboard's
/// only entry path on a Personal box.
pub(crate) const ENV_LOCAL_AUTO_LOGIN: &str = "DUDUCLAW_LOCAL_AUTO_LOGIN";

/// Which condition refused the request. Never surfaced to the caller (the HTTP
/// response is one uniform 403); it exists so tests can assert *why* a request
/// was refused and so the server can log the reason locally.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LocalSessionDenial {
    /// Operator turned the switch off.
    Disabled,
    /// Not a Personal-edition instance.
    NotPersonal,
    /// TCP peer is not a loopback address.
    OffLoopback,
    /// A proxy-injected header was present (§2.4.2 red cell).
    ProxyHeader(&'static str),
    /// `Origin` is set and not in the loopback allowlist.
    ForeignOrigin,
    /// The custom marker header is missing or wrong (§2.4.3 CSRF guard).
    MissingMarker,
}

impl LocalSessionDenial {
    /// Short stable label for the local log line. Never returned to the client.
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::NotPersonal => "not_personal",
            Self::OffLoopback => "off_loopback",
            Self::ProxyHeader(_) => "proxy_header",
            Self::ForeignOrigin => "foreign_origin",
            Self::MissingMarker => "missing_marker",
        }
    }
}

/// The first proxy-class header present in `headers`, if any.
pub(crate) fn proxy_header_present(headers: &HeaderMap) -> Option<&'static str> {
    PROXY_HEADERS
        .iter()
        .copied()
        .find(|name| headers.contains_key(*name))
}

/// Does the request carry the exact marker header the dashboard sends?
pub(crate) fn local_marker_ok(headers: &HeaderMap) -> bool {
    headers
        .get(LOCAL_MARKER_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.trim() == LOCAL_MARKER_VALUE)
        .unwrap_or(false)
}

/// The whole gate as one pure function. `Ok(())` means "issue the session".
///
/// Ordering is by blast radius, not by cost: the operator switch first (an
/// explicit "no" outranks everything), then edition, then the two transport
/// facts, then the two browser-side guards.
pub(crate) fn evaluate(
    enabled: bool,
    is_personal: bool,
    peer_is_loopback: bool,
    origin_allowed: bool,
    headers: &HeaderMap,
) -> Result<(), LocalSessionDenial> {
    if !enabled {
        return Err(LocalSessionDenial::Disabled);
    }
    if !is_personal {
        return Err(LocalSessionDenial::NotPersonal);
    }
    if !peer_is_loopback {
        return Err(LocalSessionDenial::OffLoopback);
    }
    if let Some(name) = proxy_header_present(headers) {
        return Err(LocalSessionDenial::ProxyHeader(name));
    }
    if !origin_allowed {
        return Err(LocalSessionDenial::ForeignOrigin);
    }
    if !local_marker_ok(headers) {
        return Err(LocalSessionDenial::MissingMarker);
    }
    Ok(())
}

/// Parse one switch value. Accepts a TOML boolean or the string forms the
/// design doc uses (`"auto"` / `"off"`). Returns `None` for anything
/// unrecognised so resolution falls through to the next source rather than
/// guessing.
pub(crate) fn parse_switch(value: &toml::Value) -> Option<bool> {
    match value {
        toml::Value::Boolean(b) => Some(*b),
        toml::Value::String(s) => parse_switch_str(s),
        _ => None,
    }
}

/// Shared string parsing for the env var and the TOML string form.
pub(crate) fn parse_switch_str(raw: &str) -> Option<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "on" | "yes" | "auto" => Some(true),
        "0" | "false" | "off" | "no" => Some(false),
        _ => None,
    }
}

/// Read the switch out of `config.toml`.
///
/// Two accepted keys, both `local_auto_login`: `[dashboard]` (the WP-F1 task
/// spec) wins over `[gateway]` (the name used in §2.3 of the design doc). They
/// were specified independently and an operator following either document must
/// get the behaviour they asked for; silently honouring only one would be a
/// config that "does nothing" with no diagnostic.
pub(crate) fn config_switch(home: &Path) -> Option<bool> {
    let content = std::fs::read_to_string(home.join("config.toml")).ok()?;
    let table: toml::Table = content.parse().ok()?;
    for section in ["dashboard", "gateway"] {
        let found = table
            .get(section)
            .and_then(|v| v.as_table())
            .and_then(|t| t.get("local_auto_login"))
            .and_then(parse_switch);
        if found.is_some() {
            return found;
        }
    }
    None
}

/// Effective switch state: env override > `config.toml` > default **on**.
///
/// Defaulting to on is what makes a fresh Personal install passwordless with
/// zero configuration; every *other* condition of the gate still has to hold,
/// so "on by default" never means "open by default".
pub(crate) fn auto_login_enabled(home: &Path) -> bool {
    if let Some(v) = std::env::var(ENV_LOCAL_AUTO_LOGIN)
        .ok()
        .as_deref()
        .and_then(parse_switch_str)
    {
        return v;
    }
    config_switch(home).unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A request exactly as the dashboard sends it: marker header, loopback
    /// `Origin`, nothing else.
    fn good_headers() -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(LOCAL_MARKER_HEADER, "1".parse().unwrap());
        h.insert("origin", "http://127.0.0.1:18789".parse().unwrap());
        h
    }

    fn with_header(name: &'static str, value: &str) -> HeaderMap {
        let mut h = good_headers();
        h.insert(name, value.parse().unwrap());
        h
    }

    #[test]
    fn happy_path_issues_a_session() {
        assert_eq!(evaluate(true, true, true, true, &good_headers()), Ok(()));
    }

    // ── Rejection branch 1: proxy headers (§2.4.2, the red cell) ──────────

    #[test]
    fn xff_is_refused_even_from_a_loopback_peer() {
        // The whole point: nginx on the same box makes `peer_is_loopback`
        // true while the real client is remote. Presence of the header is the
        // refusal — the value is never parsed or trusted.
        assert_eq!(
            evaluate(
                true,
                true,
                true,
                true,
                &with_header("x-forwarded-for", "203.0.113.7")
            ),
            Err(LocalSessionDenial::ProxyHeader("x-forwarded-for")),
        );
    }

    #[test]
    fn a_forged_loopback_xff_value_does_not_help_the_attacker() {
        // Someone who knows about the check might try to make the header look
        // innocent. Existence-based refusal is immune to the value.
        assert_eq!(
            evaluate(
                true,
                true,
                true,
                true,
                &with_header("x-forwarded-for", "127.0.0.1")
            ),
            Err(LocalSessionDenial::ProxyHeader("x-forwarded-for")),
        );
    }

    #[test]
    fn every_proxy_header_variant_is_refused() {
        for name in PROXY_HEADERS {
            let headers = with_header(name, "anything");
            assert_eq!(
                evaluate(true, true, true, true, &headers),
                Err(LocalSessionDenial::ProxyHeader(name)),
                "{name} must be refused",
            );
        }
    }

    #[test]
    fn empty_valued_proxy_header_is_still_refused() {
        assert_eq!(
            evaluate(true, true, true, true, &with_header("x-real-ip", "")),
            Err(LocalSessionDenial::ProxyHeader("x-real-ip")),
        );
    }

    // ── Rejection branch 2: non-loopback peer ────────────────────────────

    #[test]
    fn off_loopback_peer_is_refused() {
        assert_eq!(
            evaluate(true, true, false, true, &good_headers()),
            Err(LocalSessionDenial::OffLoopback),
        );
    }

    #[test]
    fn off_loopback_peer_cannot_talk_its_way_in_with_the_marker_header() {
        // The marker is a CSRF guard, not a credential; it must not upgrade a
        // remote caller.
        let mut h = HeaderMap::new();
        h.insert(LOCAL_MARKER_HEADER, "1".parse().unwrap());
        assert_eq!(
            evaluate(true, true, false, true, &h),
            Err(LocalSessionDenial::OffLoopback),
        );
    }

    // ── Rejection branch 3: non-Personal edition ─────────────────────────

    #[test]
    fn non_personal_edition_is_refused_even_on_loopback() {
        assert_eq!(
            evaluate(true, false, true, true, &good_headers()),
            Err(LocalSessionDenial::NotPersonal),
        );
    }

    // ── Remaining conditions ─────────────────────────────────────────────

    #[test]
    fn switch_off_refuses_before_anything_else() {
        assert_eq!(
            evaluate(false, true, true, true, &good_headers()),
            Err(LocalSessionDenial::Disabled),
        );
    }

    #[test]
    fn missing_or_wrong_marker_is_refused() {
        let mut bare = HeaderMap::new();
        bare.insert("origin", "http://127.0.0.1:18789".parse().unwrap());
        assert_eq!(
            evaluate(true, true, true, true, &bare),
            Err(LocalSessionDenial::MissingMarker),
        );

        let mut wrong = HeaderMap::new();
        wrong.insert(LOCAL_MARKER_HEADER, "0".parse().unwrap());
        assert_eq!(
            evaluate(true, true, true, true, &wrong),
            Err(LocalSessionDenial::MissingMarker),
        );
    }

    #[test]
    fn marker_tolerates_surrounding_whitespace_only() {
        let mut h = HeaderMap::new();
        h.insert(LOCAL_MARKER_HEADER, " 1 ".parse().unwrap());
        assert!(local_marker_ok(&h));
        h.insert(LOCAL_MARKER_HEADER, "11".parse().unwrap());
        assert!(!local_marker_ok(&h));
    }

    #[test]
    fn foreign_origin_is_refused() {
        assert_eq!(
            evaluate(true, true, true, false, &good_headers()),
            Err(LocalSessionDenial::ForeignOrigin),
        );
    }

    // ── Switch parsing / config resolution ───────────────────────────────

    #[test]
    fn switch_strings_and_bools_parse() {
        assert_eq!(parse_switch_str("auto"), Some(true));
        assert_eq!(parse_switch_str("OFF"), Some(false));
        assert_eq!(parse_switch_str(" 0 "), Some(false));
        assert_eq!(parse_switch_str("maybe"), None);
        assert_eq!(parse_switch(&toml::Value::Boolean(false)), Some(false));
        assert_eq!(
            parse_switch(&toml::Value::String("off".into())),
            Some(false)
        );
        assert_eq!(parse_switch(&toml::Value::Integer(0)), None);
    }

    #[test]
    fn config_switch_reads_both_accepted_sections() {
        let dir = tempfile::tempdir().unwrap();
        // Task-spec key.
        std::fs::write(
            dir.path().join("config.toml"),
            "[dashboard]\nlocal_auto_login = false\n",
        )
        .unwrap();
        assert_eq!(config_switch(dir.path()), Some(false));

        // Design-doc key (§2.3) with the documented string form.
        std::fs::write(
            dir.path().join("config.toml"),
            "[gateway]\nlocal_auto_login = \"off\"\n",
        )
        .unwrap();
        assert_eq!(config_switch(dir.path()), Some(false));

        // `[dashboard]` wins when both are present.
        std::fs::write(
            dir.path().join("config.toml"),
            "[dashboard]\nlocal_auto_login = true\n\n[gateway]\nlocal_auto_login = false\n",
        )
        .unwrap();
        assert_eq!(config_switch(dir.path()), Some(true));
    }

    #[test]
    fn missing_or_unparseable_config_leaves_the_default() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(config_switch(dir.path()), None);
        std::fs::write(dir.path().join("config.toml"), "not = = toml").unwrap();
        assert_eq!(config_switch(dir.path()), None);
        std::fs::write(
            dir.path().join("config.toml"),
            "[dashboard]\nlocal_auto_login = 3\n",
        )
        .unwrap();
        assert_eq!(config_switch(dir.path()), None);
    }
}
