//! B5 — server-initiated dashboard navigation (Wave 3, HA `command_webview`
//! pattern: the server tells a connected client where to go, the same idea
//! Home Assistant's frontend `browser_mod`/`command_webview` service uses).
//!
//! ## The gap this closes
//!
//! Every existing HITL push (`approval_notify`, `goal_notify`,
//! `install_notify`) reaches a messaging channel with buttons, but an
//! operator who already has the dashboard open in a browser tab gets nothing
//! beyond whatever the next passive poll happens to catch. This module lets
//! the gateway push a one-shot "jump to this page" event to every connected,
//! authenticated dashboard tab, over the SAME broadcast channel every other
//! `WsFrame::Event` already rides — see `dashboard_feedback.rs` for the
//! sibling "refetch this list" pattern; this one commands a route change
//! instead of a refetch, so it does not reuse that module's whitelist (a
//! navigation and a refetch signal are different shapes and different
//! consumers on the frontend).
//!
//! ## Wiring
//!
//! [`init`] is called once at gateway startup (`server.rs`, right next to
//! `Handler::set_event_tx`) with the exact `broadcast::Sender<String>` every
//! `/ws` connection subscribes to (`state.event_tx.subscribe()` in the
//! WebSocket handler — "Outbound event broadcast, always active for
//! authenticated clients"). Any later caller anywhere in the gateway process
//! reaches every open dashboard tab through [`push_dashboard_navigate`],
//! without needing a `Handler`/`ReplyContext` handle.
//!
//! First use case: [`crate::approval::ApprovalBroker`]'s ⅔-TTL "about to
//! auto-deny" reminder point calls this at the exact moment the channel
//! reminder also fires, so an admin already staring at the dashboard gets
//! routed straight to the inbox row instead of having to separately notice
//! the channel ping.
//!
//! ## Frontend contract
//!
//! `web/src/lib/dashboard-navigate.ts` subscribes to the `dashboard.navigate`
//! event once (`App.tsx`) and calls `useNavigate()` with `payload.path` — a
//! same-origin relative path only (e.g. `/inbox?item=<id>`). Validated on
//! both ends: this module never emits anything but a leading-`/`,
//! non-protocol-relative path (see [`is_safe_relative_path`]), and the
//! frontend independently re-validates before ever calling `navigate()` —
//! defense in depth, so a future caller of this module cannot steer an open
//! dashboard tab off-origin by construction on either side.

use std::sync::OnceLock;

use tokio::sync::broadcast;
use tracing::warn;

use crate::protocol::WsFrame;

/// The event name the frontend (`web/src/lib/dashboard-navigate.ts`) matches on.
pub const EVENT_NAVIGATE: &str = "dashboard.navigate";

/// Hard cap on the path length forwarded to the wire — generous for any real
/// in-app route/query string, small enough that a caller mistake (e.g.
/// accidentally formatting an entire error message into the path) cannot
/// balloon into an oversized broadcast frame.
const MAX_PATH_LEN: usize = 512;

static DASHBOARD_EVENT_TX: OnceLock<broadcast::Sender<String>> = OnceLock::new();

/// Register the process-wide dashboard WebSocket broadcast sender. Idempotent
/// (first call wins) — safe to call exactly once at startup even though
/// nothing enforces "only once" at the type level.
pub fn init(tx: broadcast::Sender<String>) {
    let _ = DASHBOARD_EVENT_TX.set(tx);
}

/// Push a `dashboard.navigate` event to every connected, authenticated
/// dashboard tab.
///
/// Best-effort and silent otherwise: no receivers (no dashboard open), a
/// not-yet-initialized sender (a code path that races gateway startup, or a
/// unit test), or an unsafe `path` are all normal no-ops — navigation is a
/// UX nicety, never load-bearing for the action it accompanies. An unsafe
/// path is logged (`warn!`) since that indicates a caller mistake, not a
/// runtime condition; everything else is silent by design (matches
/// `dashboard_feedback::emit`'s "best-effort, never fatal" contract).
pub fn push_dashboard_navigate(path: &str) {
    if !is_safe_relative_path(path) {
        warn!(path, "push_dashboard_navigate: rejected a non-relative/unsafe path");
        return;
    }
    let Some(tx) = DASHBOARD_EVENT_TX.get() else {
        return;
    };
    let frame = WsFrame::event(EVENT_NAVIGATE, serde_json::json!({ "path": path }));
    if let Ok(json) = serde_json::to_string(&frame) {
        let _ = tx.send(json);
    }
}

/// Same-origin relative path only: non-empty, bounded length, starts with
/// exactly one `/` (never `//` — protocol-relative, the classic open-redirect
/// shape), and free of control characters (no smuggling a newline/CR into a
/// log line or the wire).
fn is_safe_relative_path(path: &str) -> bool {
    if path.is_empty() || path.len() > MAX_PATH_LEN {
        return false;
    }
    if !path.starts_with('/') || path.starts_with("//") {
        return false;
    }
    !path.chars().any(|c| c.is_control())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_relative_path_accepts_and_rejects() {
        assert!(is_safe_relative_path("/inbox"));
        assert!(is_safe_relative_path("/inbox?item=ap-abc123"));
        assert!(!is_safe_relative_path(""));
        assert!(!is_safe_relative_path("inbox"), "must have a leading slash");
        assert!(!is_safe_relative_path("//evil.com"), "protocol-relative must be rejected");
        assert!(!is_safe_relative_path("/a\nb"), "control chars must be rejected");
        assert!(!is_safe_relative_path(&format!("/{}", "a".repeat(600))), "must be length-bounded");
    }

    /// The only test in the crate that touches [`DASHBOARD_EVENT_TX`] — the
    /// `OnceLock` is process-global and first-call-wins, so a second test
    /// calling `init()` with a different sender would silently make this one
    /// flaky (see module docs). Keep every assertion about the live wiring
    /// here, in one function.
    #[test]
    fn push_dashboard_navigate_wiring() {
        let (tx, mut rx) = broadcast::channel(8);
        init(tx.clone());
        // A second `init` call must be a no-op (first call wins) — otherwise
        // any other test in this binary that happened to call `init()` first
        // would silently steal this test's channel.
        init(tx);

        push_dashboard_navigate("/inbox?item=ap-1");
        let json = rx.try_recv().expect("frame sent for a safe path");
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["type"], "event");
        assert_eq!(parsed["event"], EVENT_NAVIGATE);
        assert_eq!(parsed["payload"]["path"], "/inbox?item=ap-1");

        push_dashboard_navigate("//evil.com");
        assert!(rx.try_recv().is_err(), "protocol-relative path must not be forwarded");

        push_dashboard_navigate("no-leading-slash");
        assert!(rx.try_recv().is_err(), "non-relative path must not be forwarded");
    }
}
