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

/// Build a full, clickable deep link landing on the page that shows `id`, or
/// `None` when no dashboard base URL is resolvable — callers must keep their
/// existing link-less text unchanged in that case, never emit
/// an empty/dangling link.
pub fn deep_link(home_dir: &Path, kind: DeepLinkKind, id: &str) -> Option<String> {
    let base = dashboard_base_url(home_dir)?;
    let path: String = match kind {
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
    fn base_url_prefers_public_url_over_port() {
        let dir = tempfile::tempdir().unwrap();
        write_config(
            dir.path(),
            "[dashboard]\npublic_url = \"https://ai.example.com/\"\n\n[gateway]\nport = 18789\n",
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
        write_config(dir.path(), "[gateway]\nbind = \"127.0.0.1\"\nport = 18789\n");
        assert_eq!(
            dashboard_base_url(dir.path()),
            Some("http://localhost:18789".to_string())
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
        write_config(dir.path(), "[gateway]\nport = 18789\n");
        assert_eq!(
            deep_link(dir.path(), DeepLinkKind::Task, "abc123"),
            Some("http://localhost:18789/tasks/abc123".to_string())
        );
    }

    #[test]
    fn deep_link_approval_lands_on_inbox_with_item_query() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), "[gateway]\nport = 18789\n");
        assert_eq!(
            deep_link(dir.path(), DeepLinkKind::Approval, "ap-1"),
            Some("http://localhost:18789/inbox?item=ap-1".to_string())
        );
    }

    #[test]
    fn deep_link_channels_and_billing_land_on_manage_subpages() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), "[gateway]\nport = 18789\n");
        assert_eq!(
            deep_link(dir.path(), DeepLinkKind::Channels, ""),
            Some("http://localhost:18789/manage/channels".to_string())
        );
        assert_eq!(
            deep_link(dir.path(), DeepLinkKind::Billing, ""),
            Some("http://localhost:18789/manage/billing".to_string())
        );
    }

    #[test]
    fn deep_link_system_lands_on_manage_logs() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), "[gateway]\nport = 18789\n");
        assert_eq!(
            deep_link(dir.path(), DeepLinkKind::System, ""),
            Some("http://localhost:18789/manage/logs".to_string())
        );
    }

    #[test]
    fn deep_link_autopilot_lands_on_the_autopilot_settings_tab() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), "[gateway]\nport = 18789\n");
        assert_eq!(
            deep_link(dir.path(), DeepLinkKind::Autopilot, "rule-1"),
            Some("http://localhost:18789/manage/system?tab=autopilot".to_string())
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
