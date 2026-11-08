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
//! ## Base URL resolution (in priority order)
//!
//! 1. `config.toml [dashboard] public_url` — an operator-declared externally
//!    reachable address (reverse proxy, tailnet hostname, custom domain).
//! 2. `config.toml [gateway] port` — falls back to `http://localhost:<port>`,
//!    which is only correct when the channel recipient is on the same machine
//!    as the gateway, but is still strictly better than no link at all for the
//!    common single-operator deployment. `[gateway] port` is written by
//!    `duduclaw_core::write_minimal_config` on first boot (`config.rs`) so it
//!    is present on every install that has ever started via `duduclaw run`.
//! 3. Neither present/parseable → `None`. Callers MUST treat `None` as "keep
//!    the current plain-text behavior unchanged" — never emit a dangling or
//!    empty URL (project convention: security/UX gates fail closed, and here
//!    that means fail-quiet rather than fail-ugly).
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

/// Resolve the dashboard's externally-reachable base URL (no trailing slash),
/// or `None` when neither `[dashboard] public_url` nor `[gateway] port` is
/// present/parseable in `config.toml`. See module docs for the priority order.
pub(crate) fn dashboard_base_url(home_dir: &Path) -> Option<String> {
    let content = std::fs::read_to_string(home_dir.join("config.toml")).ok()?;
    let table: toml::Table = content.parse().ok()?;

    if let Some(url) = table
        .get("dashboard")
        .and_then(|v| v.as_table())
        .and_then(|d| d.get("public_url"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return Some(url.trim_end_matches('/').to_string());
    }

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

/// Build a full, clickable deep link landing on the page that shows `id`, or
/// `None` when no dashboard base URL is resolvable — callers must keep their
/// existing link-less text unchanged in that case, never emit
/// an empty/dangling link.
pub fn deep_link(home_dir: &Path, kind: DeepLinkKind, id: &str) -> Option<String> {
    let base = dashboard_base_url(home_dir)?;
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

    #[test]
    fn base_url_prefers_public_url_over_port() {
        let dir = tempfile::tempdir().unwrap();
        write_config(
            dir.path(),
            "[dashboard]\npublic_url = \"https://ai.example.com/\"\n\n[gateway]\nport = 18889\n",
        );
        assert_eq!(
            dashboard_base_url(dir.path()),
            Some("https://ai.example.com".to_string()),
            "trailing slash must be trimmed"
        );
    }

    #[test]
    fn base_url_falls_back_to_localhost_port() {
        let dir = tempfile::tempdir().unwrap();
        write_config(
            dir.path(),
            "[gateway]\nbind = \"127.0.0.1\"\nport = 18889\n",
        );
        assert_eq!(
            dashboard_base_url(dir.path()),
            Some("http://localhost:18889".to_string())
        );
    }

    #[test]
    fn base_url_none_when_config_missing() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(dashboard_base_url(dir.path()), None);
    }

    #[test]
    fn base_url_none_when_neither_section_present() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), "[general]\nlog_level = \"info\"\n");
        assert_eq!(dashboard_base_url(dir.path()), None);
    }

    #[test]
    fn base_url_ignores_blank_public_url_and_falls_back() {
        let dir = tempfile::tempdir().unwrap();
        write_config(
            dir.path(),
            "[dashboard]\npublic_url = \"   \"\n\n[gateway]\nport = 9999\n",
        );
        assert_eq!(
            dashboard_base_url(dir.path()),
            Some("http://localhost:9999".to_string())
        );
    }

    #[test]
    fn base_url_none_on_out_of_range_port() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), "[gateway]\nport = 0\n");
        assert_eq!(dashboard_base_url(dir.path()), None);
        let dir2 = tempfile::tempdir().unwrap();
        write_config(dir2.path(), "[gateway]\nport = 70000\n");
        assert_eq!(dashboard_base_url(dir2.path()), None);
    }

    #[test]
    fn base_url_none_on_malformed_toml() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), "this is not [ valid toml");
        assert_eq!(dashboard_base_url(dir.path()), None);
    }

    #[test]
    fn deep_link_task_lands_on_the_task_detail_route() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), "[gateway]\nport = 18889\n");
        assert_eq!(
            deep_link(dir.path(), DeepLinkKind::Task, "abc123"),
            Some("http://localhost:18889/tasks/abc123".to_string())
        );
    }

    #[test]
    fn deep_link_approval_lands_on_inbox_with_item_query() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), "[gateway]\nport = 18889\n");
        assert_eq!(
            deep_link(dir.path(), DeepLinkKind::Approval, "ap-1"),
            Some("http://localhost:18889/inbox?item=ap-1".to_string())
        );
    }

    #[test]
    fn deep_link_channels_and_billing_land_on_manage_subpages() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), "[gateway]\nport = 18889\n");
        assert_eq!(
            deep_link(dir.path(), DeepLinkKind::Channels, ""),
            Some("http://localhost:18889/manage/channels".to_string())
        );
        assert_eq!(
            deep_link(dir.path(), DeepLinkKind::Billing, ""),
            Some("http://localhost:18889/manage/billing".to_string())
        );
    }

    #[test]
    fn deep_link_system_lands_on_manage_logs() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), "[gateway]\nport = 18889\n");
        assert_eq!(
            deep_link(dir.path(), DeepLinkKind::System, ""),
            Some("http://localhost:18889/manage/logs".to_string())
        );
    }

    #[test]
    fn deep_link_autopilot_lands_on_the_autopilot_settings_tab() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), "[gateway]\nport = 18889\n");
        assert_eq!(
            deep_link(dir.path(), DeepLinkKind::Autopilot, "rule-1"),
            Some("http://localhost:18889/manage/system?tab=autopilot".to_string())
        );
    }

    #[test]
    fn deep_link_none_when_no_base_url_resolvable() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(deep_link(dir.path(), DeepLinkKind::Task, "abc123"), None);
    }

    #[test]
    fn deep_link_uses_public_url_when_configured() {
        let dir = tempfile::tempdir().unwrap();
        write_config(
            dir.path(),
            "[dashboard]\npublic_url = \"https://ai.example.com\"\n",
        );
        assert_eq!(
            deep_link(dir.path(), DeepLinkKind::Task, "t-9"),
            Some("https://ai.example.com/tasks/t-9".to_string())
        );
    }
}
