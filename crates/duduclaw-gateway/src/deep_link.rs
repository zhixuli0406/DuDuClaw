//! Deep-link helper for channel-side "please open the dashboard" prompts.
//!
//! ## The gap this closes
//!
//! Every "please go to the dashboard" line in the three notify modules
//! (`goal_notify.rs`, `approval_notify.rs`, `install_notify.rs`) was, until
//! this module, plain unlinked text — a person reading it on a phone had to
//! remember or separately look up the dashboard's address. This module
//! resolves the dashboard's externally-reachable base URL from `config.toml`
//! and builds a URL that lands **on the page that actually shows the
//! object**, never on the homepage — a link that drops someone on the
//! homepage is a failed handoff.
//!
//! ## Two resolvers — pick by who reads the link
//!
//! **Links in a message delivered to a chat channel** (approvals, goal
//! notices including kickoff / observer / progress, install notices,
//! digests, budget / cost / channel alerts, reply-failure notices,
//! computer-watch links, Mini App buttons …) use ONLY
//! [`public_dashboard_base_url`] / [`public_deep_link`]:
//!
//! 1. `config.toml [dashboard] public_url`, else `[general] public_url`, else
//!    `[gateway] public_url` — **first valid wins**. Valid
//!    ([`channel_link_base`]): scheme `https` or `http` (plain http is
//!    allowed: on-premises installs are commonly reached by a LAN address),
//!    a host that is not `localhost` (any case, with or without a trailing
//!    dot, or a `*.localhost` name) nor a loopback / unspecified IP, and no
//!    user info, query or fragment. Anything else (`javascript:`, `data:`,
//!    `file:`, a bare host name …) is skipped with one warning log and the
//!    next section is tried.
//! 2. None valid and the gateway is containerized → `None`: the message
//!    carries **no link** and keeps its "open the dashboard" sentence.
//! 3. None valid on a native host → `http://localhost:<[gateway] port>`
//!    for single-machine installs. Missing or invalid ports yield `None`.
//!
//! **Links shown to a user on the same machine** (the dashboard's own system
//! status and failure logs) use [`local_dashboard_base_url`]: `[dashboard]
//! public_url` HTTP(S) values retain their original spelling and components, then
//! `http://localhost:<[gateway] port>`. It must never be used for a chat
//! message (a source-level test pins the notification modules).
//!
//! OAuth / MCP callback URLs use [`configured_public_base`] (HTTPS-only with
//! a loopback exception); neither resolver above is used for them.
//!
//! ## Object routes
//!
//! Routes are taken from the actual React Router table in `web/src/App.tsx`,
//! not invented: `tasks/:id` (`TaskDetailPage`) is a real per-object detail
//! route, so a task deep link lands exactly there. `InboxPage` (`/inbox`)
//! reads `?item=<id>` (W2-5 H5 hand-off from `07-unified-decision-design.md`
//! §6) to select the matching row and scroll it into view on load, so an
//! approval/install/kickoff deep link lands on the unified inbox with the
//! object already open — `id` is passed through verbatim (whatever the
//! caller already had: a bare broker/install-request id), and `InboxPage`
//! resolves it against its own type-prefixed item ids (`approval:<id>` /
//! `install:<id>` / ...) with a suffix match, so this module does not need to
//! know which of the three `DeepLinkKind::Approval` callers (kickoff/approval
//! broker/install) is asking. `/manage/channels`, `/manage/billing` and
//! `/manage/logs` (`LogsPage`) are existing `ManageShell` routes.

use std::path::Path;

/// The kind of object a deep link points at. `id` is meaningful for
/// [`DeepLinkKind::Task`] (`/tasks/<id>`) and [`DeepLinkKind::Approval`]
/// (`/inbox?item=<id>`, H5); it is still threaded through the other kinds so
/// call sites don't need to special-case the signature if they gain per-item
/// focus later, and so a caller can log/attribute which object triggered the
/// link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeepLinkKind {
    /// A browser's standalone page. Session context is added by the host issuer.
    ComputerSession,
    /// A task (goal-loop or manually assigned) — `/tasks/<id>`.
    Task,
    /// Anything surfaced in the unified HITL inbox: `ApprovalBroker` rows
    /// (generic approvals, goal-loop kickoff gates) and `install_requests`
    /// rows — `/inbox?item=<id>` (H5).
    Approval,
    /// Channel configuration/status — `/manage/channels`.
    Channels,
    /// Budget/spend — `/manage/billing`.
    Billing,
    /// System diagnostics / debug log stream — `/manage/logs`. The landing
    /// spot for failures that aren't fixable by an account/billing action
    /// (CLI spawn errors, timeouts, empty responses) — anywhere a human
    /// would otherwise be told to go read `~/.duduclaw/debug.log` by hand.
    System,
    /// The autopilot (automation rules) settings tab — `/manage/system?tab=autopilot`.
    /// `id` is unused (no per-rule focus support in that tab today), mirroring
    /// [`DeepLinkKind::Channels`]/[`DeepLinkKind::Billing`].
    Autopilot,
}

/// The `config.toml` sections whose `public_url` names the dashboard's
/// externally reachable address, in priority order. `[dashboard]` is what
/// the dashboard's settings page writes; `[general]` and `[gateway]` are
/// accepted as aliases (operators set them by hand, and account invitations
/// historically read `[gateway]`).
pub(crate) const PUBLIC_URL_SECTIONS: [&str; 3] = ["dashboard", "general", "gateway"];

/// One `public_url` entry as written, in [`PUBLIC_URL_SECTIONS`] order.
/// Blank strings are omitted; a non-string value is `Err(())`.
pub(crate) fn public_url_entries(table: &toml::Table) -> Vec<(&'static str, Result<&str, ()>)> {
    PUBLIC_URL_SECTIONS
        .iter()
        .filter_map(|section| {
            let value = table.get(*section)?.as_table()?.get("public_url")?;
            match value.as_str() {
                Some(raw) if raw.trim().is_empty() => None,
                Some(raw) => Some((*section, Ok(raw.trim()))),
                None => Some((*section, Err(()))),
            }
        })
        .collect()
}

/// A plain `http(s)://host[:port][/prefix]` base: no user info (not even an
/// empty `@`), query or fragment, no whitespace / control characters or
/// backslashes. Returns the parsed URL; the host is not judged here.
fn plain_http_base(raw: &str) -> Option<url::Url> {
    let raw = raw.trim();
    if raw.is_empty()
        || raw.chars().any(|c| c.is_control() || c.is_whitespace())
        || raw.contains('\\')
    {
        return None;
    }
    // `scheme://` must be written out (`http:host` is not accepted), and an
    // `@` in the authority is user info even when the parser drops it.
    let (_, rest) = raw.split_once("://")?;
    let authority_end = rest.find(|c| matches!(c, '/' | '?' | '#')).unwrap_or(rest.len());
    if rest[..authority_end].contains('@') {
        return None;
    }
    let url = url::Url::parse(raw).ok()?;
    (matches!(url.scheme(), "http" | "https")
        && url.host().is_some()
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none())
    .then_some(url)
}

/// Whether `host` can be reached by someone who is not on the gateway's own
/// machine: not `localhost` / `*.localhost` (any case, trailing dot or not),
/// not a loopback or unspecified IPv4 / IPv6 address (IPv4-mapped included).
fn host_is_off_machine(host: url::Host<&str>) -> bool {
    match host {
        url::Host::Domain(name) => {
            let name = name.trim_end_matches('.').to_ascii_lowercase();
            !name.is_empty() && name != "localhost" && !name.ends_with(".localhost")
        }
        url::Host::Ipv4(ip) => !ip.is_loopback() && !ip.is_unspecified(),
        url::Host::Ipv6(ip) => {
            !ip.is_loopback()
                && !ip.is_unspecified()
                && ip.to_ipv4().is_none_or(|v4| !v4.is_loopback() && !v4.is_unspecified())
        }
    }
}

/// The base (no trailing slash, path prefix kept) a configured `public_url`
/// gives a link that is put into a **chat-channel message**, or `None` when
/// the value is not acceptable there: see the module docs (`http`/`https`
/// only, an off-machine host, no user info / query / fragment).
pub(crate) fn channel_link_base(raw: &str) -> Option<String> {
    let url = plain_http_base(raw)?;
    if !host_is_off_machine(url.host()?) {
        return None;
    }
    Some(url.as_str().trim_end_matches('/').to_string())
}

/// Preserve legacy diagnostic HTTP(S) output, rejecting dangerous schemes.
/// Chat-channel validation remains exclusively in [`channel_link_base`].
fn local_link_base(raw: &str) -> Option<String> {
    let (scheme, _) = raw.split_once("://")?;
    (scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https"))
        .then(|| raw.trim_end_matches('/').to_string())
}

/// The first `public_url` (in [`PUBLIC_URL_SECTIONS`] order) that `accept`
/// takes. A present but rejected value is skipped; with `warn`, it is logged
/// once per section per process (never the value: it may hold credentials).
fn first_valid_public_url(
    table: &toml::Table,
    accept: fn(&str) -> Option<String>,
    warn: bool,
) -> Option<String> {
    use std::sync::atomic::{AtomicBool, Ordering};
    static WARNED: [AtomicBool; 3] =
        [AtomicBool::new(false), AtomicBool::new(false), AtomicBool::new(false)];
    for (section, value) in public_url_entries(table) {
        if let Some(base) = value.ok().and_then(accept) {
            return Some(base);
        }
        let index = PUBLIC_URL_SECTIONS.iter().position(|s| *s == section).unwrap_or(0);
        if warn && !WARNED[index].swap(true, Ordering::Relaxed) {
            tracing::warn!(
                section,
                "public_url is not usable in a chat-channel link (needs http(s), a host other than localhost/loopback, no user name, query or fragment); trying the next section"
            );
        }
    }
    None
}

fn read_config_table(home_dir: &Path) -> Option<toml::Table> {
    let content = std::fs::read_to_string(home_dir.join("config.toml")).ok()?;
    content.parse().ok()
}

/// **For a link shown on the gateway's own machine only** (the dashboard's
/// system status and failure logs). For chat-channel messages, use
/// [`public_dashboard_base_url`] instead.
///
/// The legacy `[dashboard] public_url` when HTTP(S), preserving
/// legacy diagnostic output (including loopback and URL components), else
/// `http://localhost:<[gateway] port>`, else `None`. No trailing slash.
pub(crate) fn local_dashboard_base_url(home_dir: &Path) -> Option<String> {
    let table = read_config_table(home_dir)?;
    if let Some(url) = table.get("dashboard")
        .and_then(|v| v.as_table())
        .and_then(|d| d.get("public_url"))
        .and_then(|v| v.as_str())
        .and_then(|raw| local_link_base(raw.trim()))
    {
        return Some(url);
    }
    localhost_base(&table)
}

fn localhost_base(table: &toml::Table) -> Option<String> {
    let port = table
        .get("gateway")
        .and_then(|v| v.as_table())
        .and_then(|g| g.get("port"))
        .and_then(|v| v.as_integer())?;
    if !(1..=65535).contains(&port) {
        return None;
    }
    Some(format!("http://localhost:{port}"))
}

/// **The one resolver for every link put into a message delivered to a chat
/// channel.** The first `public_url` in [`PUBLIC_URL_SECTIONS`] order that
/// [`channel_link_base`] accepts (no trailing slash, path prefix kept); an
/// invalid higher-priority value is skipped with a one-time warning. With no
/// valid public URL, containers get `None`; native hosts fall back to
/// `http://localhost:<[gateway] port>` when the port is valid. `None` = no
/// link; the caller keeps its "open the dashboard" sentence without one.
pub(crate) fn public_dashboard_base_url(home_dir: &Path) -> Option<String> {
    public_dashboard_base_url_with_container(home_dir, duduclaw_core::ns_guard::is_containerized)
}

fn public_dashboard_base_url_with_container(
    home_dir: &Path,
    is_containerized: impl FnOnce() -> bool,
) -> Option<String> {
    let table = read_config_table(home_dir)?;
    first_valid_public_url(&table, channel_link_base, true)
        .or_else(|| if is_containerized() { None } else { localhost_base(&table) })
}

/// A diagnostic deep link for dashboard-side failure logs; preserves the
/// legacy local resolver independently of the chat recipient's environment.
pub(crate) fn local_dashboard_deep_link(home_dir: &Path, kind: DeepLinkKind, id: &str) -> Option<String> {
    with_route(local_dashboard_base_url(home_dir)?, kind, id)
}

/// Why [`dashboard_public_base`] refused to produce a base. Both variants fail
/// closed: an OAuth flow must not start when the operator-declared address
/// cannot be honoured, because silently dropping it is exactly the RM-129 bug
/// (the browser lands on another site that shares the dashboard's domain).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PublicBaseError {
    /// `config.toml` exists but cannot be read or parsed.
    ConfigUnreadable,
    /// `[dashboard] public_url` is present but is not a plain http(s) base
    /// (credentials, query, fragment, dot segments, encoded separators …).
    InvalidPublicUrl,
    /// No public_url and the request origin itself is not a valid origin.
    InvalidRequestOrigin,
}

impl std::fmt::Display for PublicBaseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::ConfigUnreadable => "config.toml cannot be read; fix it before signing in",
            Self::InvalidPublicUrl => {
                "[dashboard] public_url requires HTTPS (HTTP only for localhost, 127.0.0.0/8 or ::1; no user name, query, fragment or `..`); fix it in Settings → General"
            }
            Self::InvalidRequestOrigin => "the dashboard origin is not a valid http(s) origin",
        })
    }
}

/// The validated `[dashboard] public_url`, or `Ok(None)` when it is unset or
/// blank (or there is no `config.toml` at all). The returned URL's path always
/// ends with exactly one `/`.
pub(crate) fn configured_public_base(home: &Path) -> Result<Option<url::Url>, PublicBaseError> {
    let text = match std::fs::read_to_string(home.join("config.toml")) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(PublicBaseError::ConfigUnreadable),
    };
    let table: toml::Table = text.parse().map_err(|_| PublicBaseError::ConfigUnreadable)?;
    let Some(value) = table.get("dashboard").and_then(|d| d.get("public_url")) else {
        return Ok(None);
    };
    let raw = value.as_str().ok_or(PublicBaseError::InvalidPublicUrl)?;
    if raw.trim().is_empty() {
        return Ok(None);
    }
    let Some((_, base)) = valid_oauth_public_base(raw) else {
        // Do not log the value: it may contain credentials or a query.
        tracing::warn!("invalid [dashboard] public_url; OAuth sign-in refused until it is fixed");
        return Err(PublicBaseError::InvalidPublicUrl);
    };
    normalized_base(&base).map(Some).ok_or(PublicBaseError::InvalidPublicUrl)
}

/// Parse `base` (no trailing slash required) into a URL whose path ends with
/// exactly one `/`.
fn normalized_base(base: &str) -> Option<url::Url> {
    let mut url = url::Url::parse(base).ok()?;
    let path = format!("{}/", url.path().trim_end_matches('/'));
    url.set_path(&path);
    Some(url)
}

/// `scheme://host[:port][/prefix]` with no trailing slash — the form callers
/// concatenate absolute routes onto (`{prefix}/oauth/mcp/callback`).
pub(crate) fn base_prefix(base: &url::Url) -> String {
    base.as_str().trim_end_matches('/').to_string()
}

/// The single source of truth for every OAuth callback / return URL built for
/// a browser (RM-129).
///
/// * `[dashboard] public_url` configured and valid → that URL (origin + path,
///   path normalised to end with `/`). It is authoritative even when its
///   origin differs from `request_origin`; the mismatch is logged once per
///   process because it usually means the operator reached the dashboard via
///   another address than the one they declared.
/// * public_url unset → `request_origin`, which the caller MUST already have
///   validated against the redirect-origin allowlist
///   (`remote_mcp::connect::plan_redirect`). This function never widens that
///   allowlist; it only decides the path.
/// * public_url present but invalid, or config unreadable → `Err` (fail
///   closed — never silently fall back to a prefix-less address).
pub(crate) fn dashboard_public_base(home: &Path, request_origin: &str) -> Result<url::Url, PublicBaseError> {
    if let Some(base) = configured_public_base(home)? {
        let configured_origin = base.origin().ascii_serialization();
        let request = url::Url::parse(request_origin.trim()).ok().map(|u| u.origin().ascii_serialization());
        if request.as_deref() != Some(configured_origin.as_str()) {
            static WARNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
            if !WARNED.swap(true, std::sync::atomic::Ordering::Relaxed) {
                tracing::warn!(
                    configured = %configured_origin,
                    "dashboard opened on an origin other than [dashboard] public_url; OAuth returns to public_url"
                );
            }
        }
        return Ok(base);
    }
    let url = url::Url::parse(request_origin.trim()).map_err(|_| PublicBaseError::InvalidRequestOrigin)?;
    if !matches!(url.scheme(), "http" | "https") || url.host().is_none() {
        return Err(PublicBaseError::InvalidRequestOrigin);
    }
    normalized_base(&url.origin().ascii_serialization()).ok_or(PublicBaseError::InvalidRequestOrigin)
}

pub(crate) fn valid_oauth_public_base(raw: &str) -> Option<(String, String)> {
    let raw = raw.trim();
    if raw.chars().any(char::is_control) || raw.contains('\\') {
        return None;
    }
    // Check before URL parsing, which normalizes literal and encoded dot segments.
    let (_, rest) = raw.split_once("://")?;
    let authority_end = rest.find(|c| matches!(c, '/' | '?' | '#')).unwrap_or(rest.len());
    if rest[..authority_end].contains('@') { return None; }
    let path = &rest[authority_end..];
    let dots = path.to_ascii_lowercase().replace("%2e", ".");
    if (!path.is_empty() && !path.starts_with('/'))
        || dots.contains("..") || dots.contains("%2f") || dots.contains("%5c")
    {
        return None;
    }
    let url = url::Url::parse(raw).ok()?;
    if !matches!(url.scheme(), "http" | "https") || url.host().is_none()
        || !url.username().is_empty() || url.password().is_some()
        || url.query().is_some() || url.fragment().is_some()
    {
        return None;
    }
    let loopback = match url.host()? {
        url::Host::Domain(host) => host == "localhost",
        url::Host::Ipv4(ip) => ip.is_loopback(),
        url::Host::Ipv6(ip) => ip.is_loopback(),
    };
    if url.scheme() == "http" && !loopback {
        return None;
    }
    Some((url.origin().ascii_serialization(), url.as_str().trim_end_matches('/').into()))
}

/// A full, clickable deep link for a **chat-channel message**, landing on the
/// page that shows `id`, built on [`public_dashboard_base_url`]: a valid
/// configured public URL in either environment, otherwise no link in a
/// container or a localhost link with the configured gateway port on a native
/// host. Callers keep their link-less text when no base is resolvable.
pub fn public_deep_link(home_dir: &Path, kind: DeepLinkKind, id: &str) -> Option<String> {
    let base = public_dashboard_base_url(home_dir)?;
    with_route(base, kind, id)
}

fn with_route(base: String, kind: DeepLinkKind, id: &str) -> Option<String> {
    let path: String = match kind {
        DeepLinkKind::ComputerSession => {
            if !duduclaw_core::is_valid_agent_id(id) { return None; }
            format!("/computer/{id}")
        }
        DeepLinkKind::Task => format!("/tasks/{id}"),
        // H5: per-item anchor. `id` here is whatever the caller already had
        // (a bare `ApprovalRecord`/`InstallRequest` id, no `approval:`/
        // `install:` prefix) — `InboxPage` resolves it with a suffix match
        // against its own prefixed item ids, so no prefix is added here.
        DeepLinkKind::Approval => format!("/inbox?item={id}"),
        DeepLinkKind::Channels => "/manage/channels".to_string(),
        DeepLinkKind::Billing => "/manage/billing".to_string(),
        DeepLinkKind::System => "/manage/logs".to_string(),
        DeepLinkKind::Autopilot => "/manage/system?tab=autopilot".to_string(),
    };
    Some(format!("{base}{path}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_config(dir: &Path, content: &str) {
        std::fs::write(dir.join("config.toml"), content).unwrap();
    }

    #[test]
    fn oauth_public_base_requires_https_except_for_loopback() {
        for raw in [
            "http://dashboard.example/agents", "http://192.168.1.20/agents",
            "http://localhost.evil.example/agents", "http://127.0.0.1.evil.example/agents",
            "http://[::ffff:127.0.0.1]/agents", "http://[::2]/agents", "http://128.0.0.1/agents",
        ] {
            assert!(valid_oauth_public_base(raw).is_none(), "{raw}");
        }
        for raw in [
            "http://localhost:18889/agents", "http://127.0.0.1/agents",
            "http://127.42.3.4:18889/agents", "http://127.255.255.255/agents",
            "http://[::1]:18889/agents", "https://dashboard.example/agents",
            "https://192.168.1.20/agents",
        ] {
            assert!(valid_oauth_public_base(raw).is_some(), "{raw}");
        }
    }

    #[test]
    fn public_base_uses_sub_path_public_url_and_normalizes_trailing_slash() {
        let dir = tempfile::tempdir().unwrap();
        for raw in ["https://example.com/agents", "https://example.com/agents/", "https://example.com/agents//"] {
            write_config(dir.path(), &format!("[dashboard]\npublic_url = {raw:?}\n"));
            let base = dashboard_public_base(dir.path(), "https://example.com").unwrap();
            assert_eq!(base.as_str(), "https://example.com/agents/", "{raw}");
            assert_eq!(base_prefix(&base), "https://example.com/agents");
        }
    }

    #[test]
    fn public_base_origin_mismatch_still_uses_public_url() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), "[dashboard]\npublic_url = \"https://example.com/agents\"\n");
        for origin in ["http://localhost:18889", "https://other.example.com", "not a url"] {
            let base = dashboard_public_base(dir.path(), origin).unwrap();
            assert_eq!(base_prefix(&base), "https://example.com/agents", "{origin}");
        }
    }

    #[test]
    fn public_base_unset_keeps_the_request_origin() {
        let dir = tempfile::tempdir().unwrap();
        // No config file, no [dashboard] section, blank public_url.
        assert_eq!(base_prefix(&dashboard_public_base(dir.path(), "https://dash.example").unwrap()), "https://dash.example");
        for config in ["[gateway]\nport = 18889\n", "[dashboard]\npublic_url = \"  \"\n"] {
            write_config(dir.path(), config);
            let base = dashboard_public_base(dir.path(), "http://localhost:18889/").unwrap();
            assert_eq!(base_prefix(&base), "http://localhost:18889", "{config}");
            assert_eq!(configured_public_base(dir.path()).unwrap(), None);
        }
        assert_eq!(dashboard_public_base(dir.path(), "javascript:alert(1)"), Err(PublicBaseError::InvalidRequestOrigin));
    }

    #[test]
    fn public_base_fails_closed_on_unreadable_config() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), "this is not [ valid toml");
        assert_eq!(dashboard_public_base(dir.path(), "https://dash.example"), Err(PublicBaseError::ConfigUnreadable));
        write_config(dir.path(), "[dashboard]\npublic_url = 42\n");
        assert_eq!(dashboard_public_base(dir.path(), "https://dash.example"), Err(PublicBaseError::InvalidPublicUrl));
    }

    #[test]
    fn invalid_oauth_public_urls_warn_and_fail_closed() {
        use std::io::Write;
        use std::sync::{Arc, Mutex};
        #[derive(Clone)]
        struct Capture(Arc<Mutex<Vec<u8>>>);
        impl Write for Capture {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
        }
        let dir = tempfile::tempdir().unwrap();
        let origin = "https://dash.example";
        for raw in [
            "https://dash.example/a/../b", "https://dash.example/a/%2e%2e/b",
            "https://dash.example/a?query=private", "https://dash.example/a#fragment",
            "https://user:secret@dash.example/a", "https://@dash.example/a", "/relative",
            "https://dash.example/a%2fb", "https://dash.example/a%5cb",
            "http://dashboard.example/agents", "http://192.168.1.20/agents",
        ] {
            write_config(dir.path(), &format!("[dashboard]\npublic_url = {raw:?}\n"));
            let output = Arc::new(Mutex::new(Vec::new()));
            let writer = Capture(output.clone());
            let subscriber = tracing_subscriber::fmt().without_time().with_ansi(false)
                .with_max_level(tracing::Level::WARN).with_writer(move || writer.clone()).finish();
            tracing::subscriber::with_default(subscriber, || {
                assert_eq!(dashboard_public_base(dir.path(), origin), Err(PublicBaseError::InvalidPublicUrl));
            });
            let log = String::from_utf8(output.lock().unwrap().clone()).unwrap();
            assert!(log.contains("invalid [dashboard] public_url"), "{raw}: {log}");
            assert!(!log.contains(raw));
        }
    }

    fn chat_base(home: &Path, containerized: bool) -> Option<String> {
        public_dashboard_base_url_with_container(home, || containerized)
    }

    fn chat_link(home: &Path, kind: DeepLinkKind, id: &str, containerized: bool) -> Option<String> {
        with_route(chat_base(home, containerized)?, kind, id)
    }

    /// A config with a valid public address and the usual gateway port.
    const PUBLIC: &str = "[dashboard]\npublic_url = \"https://ai.example.com/\"\n\n[gateway]\nport = 18889\n";

    #[test]
    fn local_base_prefers_public_url_over_port() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), PUBLIC);
        assert_eq!(
            local_dashboard_base_url(dir.path()),
            Some("https://ai.example.com".to_string()),
            "trailing slash must be trimmed"
        );
    }

    #[test]
    fn local_base_falls_back_to_localhost_port() {
        let dir = tempfile::tempdir().unwrap();
        write_config(
            dir.path(),
            "[gateway]\nbind = \"127.0.0.1\"\nport = 18889\n",
        );
        assert_eq!(
            local_dashboard_base_url(dir.path()),
            Some("http://localhost:18889".to_string())
        );
        // A loopback public_url is fine for a same-machine link.
        write_config(dir.path(), "[dashboard]\npublic_url = \"http://127.0.0.1:18889/\"\n");
        assert_eq!(local_dashboard_base_url(dir.path()).as_deref(), Some("http://127.0.0.1:18889"));
    }

    #[test]
    fn local_base_rejects_dangerous_schemes() {
        let dir = tempfile::tempdir().unwrap();
        for bad in ["javascript:alert(1)", "ai.example.com", "data:text/html,x", "file:///etc/passwd", "ftp://x.example"] {
            write_config(dir.path(), &format!("[dashboard]\npublic_url = {bad:?}\n[gateway]\nport = 18889\n"));
            assert_eq!(local_dashboard_base_url(dir.path()).as_deref(), Some("http://localhost:18889"), "{bad}");
        }
    }

    #[test]
    fn local_diagnostic_links_preserve_legacy_http_output() {
        let dir = tempfile::tempdir().unwrap();
        for raw in [
            "http://localhost:9999/", "http://127.0.0.1:9999/base/",
            "https://u:p@x.example/base?x=1#frag", "https://x.example:443/base/",
        ] {
            write_config(dir.path(), &format!("[dashboard]\npublic_url = {raw:?}\n[gateway]\nport = 9999\n"));
            let base = raw.trim_end_matches('/');
            assert_eq!(local_dashboard_base_url(dir.path()).as_deref(), Some(base));
            for kind in [DeepLinkKind::Billing, DeepLinkKind::System] {
                let route = if kind == DeepLinkKind::Billing { "/manage/billing" } else { "/manage/logs" };
                assert_eq!(local_dashboard_deep_link(dir.path(), kind, ""), Some(format!("{base}{route}")));
            }
        }
        write_config(dir.path(), "[general]\npublic_url = \"https://general.example\"\n[gateway]\npublic_url = \"https://gw.example\"\nport = 9999\n");
        assert_eq!(local_dashboard_base_url(dir.path()).as_deref(), Some("http://localhost:9999"));
        write_config(dir.path(), "[dashboard]\npublic_url = \"javascript:alert(1)\"\n[gateway]\nport = 9999\n");
        assert_eq!(local_dashboard_deep_link(dir.path(), DeepLinkKind::System, "").as_deref(), Some("http://localhost:9999/manage/logs"));
    }

    #[test]
    fn base_url_none_when_config_missing() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(local_dashboard_base_url(dir.path()), None);
        assert_eq!(public_dashboard_base_url(dir.path()), None);
    }

    #[test]
    fn base_url_none_when_neither_section_present() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), "[general]\nlog_level = \"info\"\n");
        assert_eq!(local_dashboard_base_url(dir.path()), None);
        assert_eq!(public_dashboard_base_url(dir.path()), None);
    }

    #[test]
    fn base_url_ignores_blank_public_url_and_falls_back() {
        let dir = tempfile::tempdir().unwrap();
        write_config(
            dir.path(),
            "[dashboard]\npublic_url = \"   \"\n\n[gateway]\nport = 9999\n",
        );
        assert_eq!(
            local_dashboard_base_url(dir.path()),
            Some("http://localhost:9999".to_string())
        );
        assert_eq!(chat_base(dir.path(), true), None);
        assert_eq!(chat_base(dir.path(), false).as_deref(), Some("http://localhost:9999"));
    }

    #[test]
    fn base_url_none_on_out_of_range_port() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), "[gateway]\nport = 0\n");
        assert_eq!(local_dashboard_base_url(dir.path()), None);
        let dir2 = tempfile::tempdir().unwrap();
        write_config(dir2.path(), "[gateway]\nport = 70000\n");
        assert_eq!(local_dashboard_base_url(dir2.path()), None);
    }

    #[test]
    fn base_url_none_on_malformed_toml() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), "this is not [ valid toml");
        assert_eq!(local_dashboard_base_url(dir.path()), None);
        assert_eq!(public_dashboard_base_url(dir.path()), None);
    }

    #[test]
    fn deep_link_task_lands_on_the_task_detail_route() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), PUBLIC);
        assert_eq!(
            public_deep_link(dir.path(), DeepLinkKind::Task, "abc123"),
            Some("https://ai.example.com/tasks/abc123".to_string())
        );
    }

    #[test]
    fn deep_link_approval_lands_on_inbox_with_item_query() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), PUBLIC);
        assert_eq!(
            public_deep_link(dir.path(), DeepLinkKind::Approval, "ap-1"),
            Some("https://ai.example.com/inbox?item=ap-1".to_string())
        );
    }

    #[test]
    fn deep_link_channels_and_billing_land_on_manage_subpages() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), PUBLIC);
        assert_eq!(
            public_deep_link(dir.path(), DeepLinkKind::Channels, ""),
            Some("https://ai.example.com/manage/channels".to_string())
        );
        assert_eq!(
            public_deep_link(dir.path(), DeepLinkKind::Billing, ""),
            Some("https://ai.example.com/manage/billing".to_string())
        );
    }

    #[test]
    fn deep_link_system_lands_on_manage_logs() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), PUBLIC);
        assert_eq!(
            public_deep_link(dir.path(), DeepLinkKind::System, ""),
            Some("https://ai.example.com/manage/logs".to_string())
        );
    }

    #[test]
    fn deep_link_autopilot_lands_on_the_autopilot_settings_tab() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), PUBLIC);
        assert_eq!(
            public_deep_link(dir.path(), DeepLinkKind::Autopilot, "rule-1"),
            Some("https://ai.example.com/manage/system?tab=autopilot".to_string())
        );
    }

    #[test]
    fn deep_link_none_when_no_base_url_resolvable() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(public_deep_link(dir.path(), DeepLinkKind::Task, "abc123"), None);
        write_config(dir.path(), "[gateway]\nport = 18889\n");
        assert_eq!(chat_link(dir.path(), DeepLinkKind::Task, "abc123", true), None);
        assert_eq!(chat_link(dir.path(), DeepLinkKind::Task, "abc123", false).as_deref(), Some("http://localhost:18889/tasks/abc123"));
    }

    /// Finding 2 matrix: every rule, for each of the three sections alone.
    #[test]
    fn channel_link_base_rules_apply_to_every_section() {
        let dir = tempfile::tempdir().unwrap();
        let rejected = [
            "javascript:alert(1)",
            "JavaScript:alert(1)",
            "data:text/html,<b>x</b>",
            "file:///etc/passwd",
            "ftp://files.example.com",
            "ai.example.com",
            "http:ai.example.com",
            "http://localhost",
            "http://localhost:18889",
            "http://LOCALHOST:18889/",
            "http://localhost.:18889",
            "http://app.localhost:18889",
            "https://localhost/base",
            "http://127.0.0.1",
            "http://127.0.0.1:18889/",
            "http://127.9.8.7",
            "http://0.0.0.0:18889",
            "http://[::1]",
            "http://[::1]:18889/base",
            "http://[::]:18889",
            "http://[::ffff:127.0.0.1]:18889",
            "https://user:secret@example.com/base",
            "https://user@example.com",
            "https://@example.com",
            "https://example.com/base?x=1",
            "https://example.com/base?",
            "https://example.com/base#frag",
            "https://example.com/base#",
            "https://exa mple.com",
            "http://127.1/",
            "http://2130706433/",
            "http://0x7f.1/",
            "http://0177.0.0.1/",
            "http://[::ffff:127.0.0.1]/",
            "http://0.0.0.0/",
            "http://[::]/",
            "http://example.com@localhost/",
            "http://localhost@example.com/",
            "http://example.com\\path",
            "http://example.com/line\nbreak",
            "http://example.com/line\u{0001}break",
            "http://localhost./",
        ];
        let accepted = [
            ("http://192.168.1.10:18889", "http://192.168.1.10:18889"),
            ("http://192.168.1.10:18889/", "http://192.168.1.10:18889"),
            ("https://example.com/base/", "https://example.com/base"),
            ("https://example.com/base", "https://example.com/base"),
            ("https://example.com", "https://example.com"),
            ("http://dashboard.lan:8080/duduclaw/", "http://dashboard.lan:8080/duduclaw"),
            ("HTTPS://Example.com", "https://example.com"),
            ("https://例え.テスト/", "https://xn--r8jz45g.xn--zckzah"),
            ("https://example.com/a/../b/", "https://example.com/b"),
        ];
        for section in PUBLIC_URL_SECTIONS {
            // Invalid configured values fall through to the environment-specific fallback.
            let port = if section == "gateway" { "port = 18889\n" } else { "[gateway]\nport = 18889\n" };
            for bad in rejected {
                assert_eq!(channel_link_base(bad), None, "{bad}");
                let value = toml::Value::String(bad.to_string());
                write_config(dir.path(), &format!("[{section}]\npublic_url = {value}\n{port}"));
                assert_eq!(chat_base(dir.path(), true), None, "[{section}] {bad}");
                assert_eq!(chat_link(dir.path(), DeepLinkKind::Task, "t", true), None, "[{section}] {bad}");
                assert_eq!(chat_base(dir.path(), false).as_deref(), Some("http://localhost:18889"), "[{section}] {bad}");
                assert_eq!(chat_link(dir.path(), DeepLinkKind::Task, "t", false).as_deref(), Some("http://localhost:18889/tasks/t"), "[{section}] {bad}");
            }
            for (good, base) in accepted {
                write_config(dir.path(), &format!("[{section}]\npublic_url = {good:?}\n{port}"));
                for containerized in [false, true] {
                    assert_eq!(chat_base(dir.path(), containerized).as_deref(), Some(base), "[{section}] {good}");
                    assert_eq!(
                        chat_link(dir.path(), DeepLinkKind::Task, "t-1", containerized),
                        Some(format!("{base}/tasks/t-1")),
                        "[{section}] {good}: the path prefix is preserved"
                    );
                }
            }
        }
    }

    #[test]
    fn an_invalid_higher_priority_public_url_falls_through_to_a_valid_lower_one() {
        let dir = tempfile::tempdir().unwrap();
        let cases = [
            ("[dashboard]\npublic_url = \"javascript:alert(1)\"\n[general]\npublic_url = \"https://general.example\"\n", "https://general.example"),
            ("[dashboard]\npublic_url = \"http://localhost:18889\"\n[gateway]\npublic_url = \"http://192.168.1.10:18889\"\n", "http://192.168.1.10:18889"),
            ("[dashboard]\npublic_url = \"http://127.0.0.1\"\n[general]\npublic_url = \"https://u:p@x.example\"\n[gateway]\npublic_url = \"https://gw.example/base/\"\n", "https://gw.example/base"),
            ("[general]\npublic_url = \"http://[::1]\"\n[gateway]\npublic_url = \"https://gw.example\"\n", "https://gw.example"),
        ];
        for (config, want) in cases {
            write_config(dir.path(), config);
            for containerized in [false, true] {
                assert_eq!(chat_base(dir.path(), containerized).as_deref(), Some(want), "{config}");
            }
        }
        // Nothing valid anywhere → container suppression or native localhost.
        write_config(
            dir.path(),
            "[dashboard]\npublic_url = \"http://localhost:18889\"\n[general]\npublic_url = \"javascript:alert(1)\"\n[gateway]\npublic_url = \"http://[::1]\"\nport = 18889\n",
        );
        assert_eq!(chat_base(dir.path(), true), None);
        assert_eq!(chat_base(dir.path(), false).as_deref(), Some("http://localhost:18889"));
    }

    /// The diagnostic resolver never feeds a chat-channel message:
    /// none of the modules that build channel notifications names it.
    #[test]
    fn channel_notification_modules_do_not_use_the_local_resolver() {
        let sources = [
            ("approval_notify.rs", include_str!("approval_notify.rs")),
            ("goal_notify.rs", include_str!("goal_notify.rs")),
            ("goal_notify_private.rs", include_str!("goal_notify_private.rs")),
            ("install_notify.rs", include_str!("install_notify.rs")),
            ("decision_notify.rs", include_str!("decision_notify.rs")),
            ("notify_push.rs", include_str!("notify_push.rs")),
            ("notify_digest.rs", include_str!("notify_digest.rs")),
            ("digest.rs", include_str!("digest.rs")),
            ("autopilot_approval.rs", include_str!("autopilot_approval.rs")),
            ("autopilot_notify.rs", include_str!("autopilot_notify.rs")),
            ("budget.rs", include_str!("budget.rs")),
            ("cost_anomaly.rs", include_str!("cost_anomaly.rs")),
            ("channel_alerts.rs", include_str!("channel_alerts.rs")),
            ("macos_folder_access.rs", include_str!("macos_folder_access.rs")),
            ("miniapp.rs", include_str!("miniapp.rs")),
            ("responsibility/notify.rs", include_str!("responsibility/notify.rs")),
            ("channel_reply/failure_message.rs", include_str!("channel_reply/failure_message.rs")),
            ("computer_watch_links/delivery.rs", include_str!("computer_watch_links/delivery.rs")),
        ];
        let local_base = ["local_", "dashboard_base_url("].concat();
        let local_deep = ["local_", "dashboard_deep_link("].concat();
        for (name, src) in sources {
            assert!(!src.contains(&local_base), "{name} must use public_dashboard_base_url / public_deep_link");
            assert!(!src.contains(&local_deep), "{name} must use public_dashboard_base_url / public_deep_link");
        }
        // The dashboard status uses the diagnostic resolver directly.
        let status = include_str!("handlers/system_status.rs");
        assert!(status.contains(&format!("crate::deep_link::{local_base}")));
    }

    #[test]
    fn base_url_reads_public_url_aliases_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let cases = [
            (
                "[dashboard]\npublic_url = \"https://dash.example\"\n[general]\npublic_url = \"https://general.example\"\n[gateway]\npublic_url = \"https://gw.example\"\nport = 18889\n",
                "https://dash.example",
            ),
            (
                "[dashboard]\npublic_url = \"  \"\n[general]\npublic_url = \"https://general.example/\"\n[gateway]\npublic_url = \"https://gw.example\"\nport = 18889\n",
                "https://general.example",
            ),
            (
                "[general]\nlog_level = \"info\"\n[gateway]\npublic_url = \"https://gw.example\"\nport = 18889\n",
                "https://gw.example",
            ),
            // A non-string value is skipped like an absent one.
            (
                "[dashboard]\npublic_url = 42\n[general]\npublic_url = \"https://general.example\"\n",
                "https://general.example",
            ),
        ];
        for (config, want) in cases {
            write_config(dir.path(), config);
            for containerized in [false, true] {
                assert_eq!(chat_base(dir.path(), containerized).as_deref(), Some(want), "{config}");
            }
        }
        // A value without a scheme is no longer passed through by either
        // resolver (it was never a clickable link).
        write_config(dir.path(), "[dashboard]\npublic_url = \"ai.example.com\"\n");
        assert_eq!(local_dashboard_base_url(dir.path()), None);
        assert_eq!(public_dashboard_base_url(dir.path()), None);
    }

    #[test]
    fn chat_base_fallback_depends_on_containerization() {
        let dir = tempfile::tempdir().unwrap();
        for containerized in [false, true] {
            assert_eq!(chat_base(dir.path(), containerized), None);
        }
        for port in [18889, 9999] {
            write_config(dir.path(), &format!("[gateway]\nport = {port}\n"));
            assert_eq!(chat_base(dir.path(), true), None);
            assert_eq!(chat_link(dir.path(), DeepLinkKind::Task, "t-1", true), None);
            assert_eq!(chat_base(dir.path(), false), Some(format!("http://localhost:{port}")));
            assert_eq!(chat_link(dir.path(), DeepLinkKind::Task, "t-1", false), Some(format!("http://localhost:{port}/tasks/t-1")));
        }
        for config in ["[gateway]\nport = 0\n", "[gateway]\nport = 70000\n", "bad [ toml", "[general]\nlog_level = \"info\"\n"] {
            write_config(dir.path(), config);
            for containerized in [false, true] {
                assert_eq!(chat_base(dir.path(), containerized), None);
            }
        }
    }

    #[test]
    fn deep_link_uses_public_url_when_configured() {
        let dir = tempfile::tempdir().unwrap();
        write_config(
            dir.path(),
            "[dashboard]\npublic_url = \"https://ai.example.com\"\n",
        );
        assert_eq!(
            public_deep_link(dir.path(), DeepLinkKind::Task, "t-9"),
            Some("https://ai.example.com/tasks/t-9".to_string())
        );
    }
}
