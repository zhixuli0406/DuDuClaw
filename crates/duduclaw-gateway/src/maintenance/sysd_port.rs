//! The three sysd verbs the maintenance SSH window uses, behind a trait so the
//! whole lifecycle can be tested against a mock (design §10.1 gateway items).
//!
//! The gateway never decides by itself whether an appliance's SSH is closed:
//! [`SysdAccess`] keeps "this host has no sysd at all" (off the appliance, the
//! only case where a close may count as "nothing to close") apart from "this
//! is an appliance and sysd does not answer" (the close is unconfirmed and is
//! recorded as `close_error`, design H1 / §4.5).

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use duduclaw_sysd::{SysdClient, SysdClientError, SysdOpOutput, SysdRequest};
use serde::Deserialize;

/// Timeout for `ssh_maint_open` and `ssh_service_stop`: both run a handful of
/// `systemctl`/`nft` commands (sysd caps each at 30 s), never a long payload.
const SSH_VERB_TIMEOUT: Duration = Duration::from_secs(120);
/// Timeout for `ssh_maint_status`, polled every 30 s by the supervisor.
const STATUS_TIMEOUT: Duration = Duration::from_secs(30);

/// Why a sysd call did not produce an answer the caller can use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SysdCallError {
    /// No connection could be made (socket missing, refused).
    Unreachable(String),
    /// sysd is older than the verb (`unknown variant`), see
    /// `device_ops::is_unknown_verb_rejection`.
    UnknownVerb,
    /// sysd answered with a structured refusal (`busy`, `unsupported`,
    /// `bad_request`, `io`, …).
    Rejected { kind: String, message: String },
    /// The call may or may not have taken effect (timeout, broken
    /// connection, undecodable reply).
    Transport(String),
}

impl std::fmt::Display for SysdCallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SysdCallError::Unreachable(e) => write!(f, "sysd unreachable: {e}"),
            SysdCallError::UnknownVerb => write!(f, "sysd does not support this verb"),
            SysdCallError::Rejected { kind, message } => {
                write!(f, "sysd refused ({kind}): {message}")
            }
            SysdCallError::Transport(e) => write!(f, "sysd call outcome unknown: {e}"),
        }
    }
}

impl SysdCallError {
    pub fn from_client(e: SysdClientError) -> Self {
        if crate::device_ops::is_unknown_verb_rejection(&e) {
            return SysdCallError::UnknownVerb;
        }
        match e {
            SysdClientError::Connect { .. } => SysdCallError::Unreachable(e.to_string()),
            SysdClientError::Rejected { kind, message } => {
                SysdCallError::Rejected { kind, message }
            }
            other => SysdCallError::Transport(other.to_string()),
        }
    }

    /// sysd was reached and answered (even if with a refusal).
    pub fn reached_sysd(&self) -> bool {
        matches!(
            self,
            SysdCallError::UnknownVerb | SysdCallError::Rejected { .. }
        )
    }
}

pub type SysdCallResult = Result<SysdOpOutput, SysdCallError>;

/// The verbs the maintenance window needs.
#[async_trait]
pub trait MaintSysd: Send + Sync {
    async fn ssh_maint_open(
        &self,
        public_key: &str,
        ttl_seconds: u64,
        window_id: &str,
    ) -> SysdCallResult;
    async fn ssh_maint_status(&self) -> SysdCallResult;
    async fn ssh_service_stop(&self) -> SysdCallResult;
}

/// Which sysd, if any, this process can talk to right now.
#[derive(Clone)]
pub enum SysdAccess {
    /// Not an appliance: no privileged daemon exists, SSH can never have been
    /// opened by this gateway.
    OffAppliance,
    /// An appliance whose sysd socket is missing.
    Unreachable(String),
    Available(Arc<dyn MaintSysd>),
}

/// Re-evaluated on every use: sysd may come up after the gateway (it is
/// `Type=simple`) or disappear while a window is open.
pub trait SysdProvider: Send + Sync {
    fn access(&self) -> SysdAccess;
}

/// The real sysd over its Unix socket.
pub struct SocketMaintSysd {
    client: SysdClient,
}

impl SocketMaintSysd {
    pub fn from_env() -> Self {
        Self {
            client: SysdClient::from_env(),
        }
    }

    async fn call(&self, req: &SysdRequest, timeout: Duration) -> SysdCallResult {
        self.client
            .clone()
            .with_timeout(timeout)
            .call(req)
            .await
            .map_err(SysdCallError::from_client)
    }
}

#[async_trait]
impl MaintSysd for SocketMaintSysd {
    async fn ssh_maint_open(
        &self,
        public_key: &str,
        ttl_seconds: u64,
        window_id: &str,
    ) -> SysdCallResult {
        let req = SysdRequest::SshMaintOpen {
            public_key: public_key.to_string(),
            ttl_seconds,
            window_id: window_id.to_string(),
        };
        self.call(&req, SSH_VERB_TIMEOUT).await
    }

    async fn ssh_maint_status(&self) -> SysdCallResult {
        self.call(&SysdRequest::SshMaintStatus, STATUS_TIMEOUT)
            .await
    }

    async fn ssh_service_stop(&self) -> SysdCallResult {
        self.call(&SysdRequest::SshServiceStop, SSH_VERB_TIMEOUT)
            .await
    }
}

/// Production provider: appliance check plus socket presence. The gateway
/// process talks to the socket directly (the `sysd_route` forwarding is only
/// for the `mcp-server` process, and these verbs are not forwardable).
pub struct SystemSysdProvider;

impl SysdProvider for SystemSysdProvider {
    fn access(&self) -> SysdAccess {
        if !duduclaw_core::is_appliance() {
            return SysdAccess::OffAppliance;
        }
        let socket = duduclaw_sysd::resolve_socket_path();
        if !socket.exists() {
            return SysdAccess::Unreachable(format!(
                "sysd socket {} does not exist",
                socket.display()
            ));
        }
        SysdAccess::Available(Arc::new(SocketMaintSysd::from_env()))
    }
}

// ── Reply shapes ────────────────────────────────────────────

/// `ssh_maint_open` stdout. Unknown fields are ignored.
#[derive(Debug, Clone, Deserialize)]
pub struct OpenReply {
    #[serde(default)]
    pub window_id: Option<String>,
    pub key_type: String,
    pub key_fingerprint: String,
    #[serde(default)]
    pub host_key_fingerprints: Vec<String>,
}

/// One session in `ssh_maint_status`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SessionView {
    #[serde(default)]
    pub pid: Option<u32>,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub fingerprint: Option<String>,
    #[serde(default)]
    pub since_usec: u64,
}

/// One event in `ssh_maint_status`. `kind` stays a string so a newer sysd's
/// extra kinds are skipped instead of failing the whole reply.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct EventView {
    #[serde(default)]
    pub cursor: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub realtime_usec: u64,
    #[serde(default)]
    pub pid: Option<u32>,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub fingerprint: Option<String>,
    #[serde(default)]
    pub message: String,
}

/// `ssh_maint_status` stdout (design §8.1).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct StatusReply {
    #[serde(default)]
    pub ready: bool,
    #[serde(default)]
    pub key_installed: Option<bool>,
    #[serde(default)]
    pub key_fingerprint: Option<String>,
    #[serde(default)]
    pub window_id: Option<String>,
    #[serde(default)]
    pub expires_in_s: Option<u64>,
    #[serde(default)]
    pub fallback_timer_active: Option<bool>,
    #[serde(default)]
    pub ssh_active: Option<bool>,
    #[serde(default)]
    pub sessions: Vec<SessionView>,
    #[serde(default)]
    pub events: Vec<EventView>,
    #[serde(default)]
    pub event_overflow: u64,
    #[serde(default)]
    pub auth_failures: Option<u64>,
    #[serde(default)]
    pub host_key_fingerprints: Vec<String>,
    #[serde(default)]
    pub pending_cleanup: bool,
    /// sysd still owes the root audit file a record (a close record or an
    /// event backfill); it refuses new opens with `busy` until it is written.
    #[serde(default)]
    pub pending_audit: bool,
    /// The most recent close sysd recorded. Current sysd persists and reports
    /// this; it remains optional for older sysd versions and a fresh install.
    #[serde(default)]
    pub last_close: Option<LastClose>,
    #[serde(default)]
    pub problems: Vec<String>,
}

/// sysd's last close record, as far as the gateway uses it.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct LastClose {
    #[serde(default)]
    pub window_id: Option<String>,
    #[serde(default)]
    pub trigger: Option<String>,
    #[serde(default)]
    pub events_complete: Option<bool>,
    #[serde(default)]
    pub events_lost: Option<bool>,
}

impl LastClose {
    /// The close trigger as one of the known tokens; anything else is
    /// `other` (sysd text never reaches the dashboard as-is).
    pub fn trigger_token(&self) -> &'static str {
        match self.trigger.as_deref() {
            Some("stop") => "stop",
            Some("deadline") => "deadline",
            Some("startup_cleanup") => "startup_cleanup",
            Some("fallback_timer") => "fallback_timer",
            Some("reboot") => "reboot",
            Some("firewall_changed") => "firewall_changed",
            None => "unknown",
            Some(_) => "other",
        }
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "window_id": sanitize_opt(&self.window_id),
            "trigger": self.trigger_token(),
            "events_complete": self.events_complete,
            "events_lost": self.events_lost.unwrap_or(false),
        })
    }
}

impl StatusReply {
    pub fn parse(stdout: &str) -> Result<Self, String> {
        serde_json::from_str(stdout.trim())
            .map_err(|e| format!("unreadable ssh_maint_status reply: {e}"))
    }

    /// sysd holds a window or a key file (either is enough to call it open).
    pub fn has_window(&self) -> bool {
        self.window_id.is_some() || self.key_installed == Some(true)
    }

    /// sysd positively says nothing is installed.
    pub fn definitely_closed(&self) -> bool {
        self.window_id.is_none() && self.key_installed == Some(false)
    }

    /// The SSH server or a session is up (`None` when sysd could not tell).
    pub fn observed_active(&self) -> Option<bool> {
        if !self.sessions.is_empty() {
            return Some(true);
        }
        self.ssh_active
    }
}

// ── Strings from sysd are data ──────────────────────────────

/// Longest text kept from a sysd/dropbear string.
pub const EVENT_TEXT_MAX_CHARS: usize = 128;
/// Longest text kept from a sysd problem line.
pub const PROBLEM_TEXT_MAX_CHARS: usize = 256;
/// Longest journal cursor accepted as a dedup key.
pub const MAX_CURSOR_LEN: usize = 512;

/// Strip control characters (C0, DEL, C1) and bidirectional overrides, then
/// cut to `max_chars` characters. sysd already does this; the gateway does it
/// again because the text reaches the dashboard and the audit file (L7).
pub fn sanitize(text: &str, max_chars: usize) -> String {
    let cleaned: String = text
        .chars()
        .filter(|c| !c.is_control() && !is_bidi_control(*c))
        .collect();
    duduclaw_core::truncate_chars(cleaned.trim(), max_chars)
}

fn is_bidi_control(c: char) -> bool {
    matches!(c, '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

#[cfg(all(test, unix))]
#[path = "tests_socket.rs"]
mod socket_tests;

pub fn sanitize_opt(text: &Option<String>) -> Option<String> {
    text.as_deref()
        .map(|t| sanitize(t, EVENT_TEXT_MAX_CHARS))
        .filter(|t| !t.is_empty())
}

/// A journal cursor usable as a dedup key: printable ASCII, no spaces, bounded.
pub fn valid_cursor(c: &str) -> bool {
    !c.is_empty() && c.len() <= MAX_CURSOR_LEN && c.bytes().all(|b| b.is_ascii_graphic())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_verb_and_connect_errors_are_classified() {
        let unknown = SysdClientError::Rejected {
            kind: "bad_request".into(),
            message: "malformed request: unknown variant `ssh_maint_status`, expected one of …"
                .into(),
        };
        assert_eq!(
            SysdCallError::from_client(unknown),
            SysdCallError::UnknownVerb
        );
        // A bad_request for another reason is not "old sysd".
        let other = SysdClientError::Rejected {
            kind: "bad_request".into(),
            message: "request line too long".into(),
        };
        assert!(matches!(
            SysdCallError::from_client(other),
            SysdCallError::Rejected { .. }
        ));
        let connect = SysdClientError::Connect {
            path: "/run/duduclaw/sysd.sock".into(),
            source: std::io::Error::from(std::io::ErrorKind::ConnectionRefused),
        };
        assert!(matches!(
            SysdCallError::from_client(connect),
            SysdCallError::Unreachable(_)
        ));
        let timeout = SysdClientError::Timeout(Duration::from_secs(1));
        assert!(matches!(
            SysdCallError::from_client(timeout),
            SysdCallError::Transport(_)
        ));
    }

    #[test]
    fn status_reply_parses_the_documented_shape_and_tolerates_extra_fields() {
        let raw = r#"{"ready":true,"key_installed":true,"key_fingerprint":"SHA256:x","window_id":"w1",
            "expires_in_s":120,"fallback_timer_active":true,"ssh_active":true,
            "sessions":[{"pid":7,"source":"10.0.0.2","fingerprint":"SHA256:x","since_usec":1}],
            "events":[{"cursor":"c1","kind":"login","realtime_usec":5,"pid":7,"source":"10.0.0.2","fingerprint":"SHA256:x","message":"m"}],
            "event_overflow":0,"auth_failures":3,"host_key_fingerprints":["SHA256:h"],
            "pending_cleanup":false,"problems":[],"future_field":1}"#;
        let st = StatusReply::parse(raw).unwrap();
        assert!(st.has_window());
        assert!(!st.definitely_closed());
        assert_eq!(st.observed_active(), Some(true));
        assert_eq!(st.auth_failures, Some(3));
        assert_eq!(st.events[0].kind, "login");
        let closed = StatusReply::parse(
            r#"{"ready":true,"key_installed":false,"window_id":null,"ssh_active":false}"#,
        )
        .unwrap();
        assert!(closed.definitely_closed());
        assert_eq!(closed.observed_active(), Some(false));
        // key_installed unknown (null) is neither open nor definitely closed.
        let rebooted = StatusReply::parse(
            r#"{"key_installed":false,"last_close":{"window_id":"w0","trigger":"reboot","events_complete":false,"events_lost":true}}"#,
        )
        .unwrap();
        let lc = rebooted.last_close.unwrap();
        assert_eq!(lc.trigger_token(), "reboot");
        assert_eq!(lc.to_json()["events_lost"], true);
        let odd = LastClose {
            trigger: Some("\u{1b}weird".into()),
            ..Default::default()
        };
        assert_eq!(odd.trigger_token(), "other");
        let unknown = StatusReply::parse(r#"{"key_installed":null}"#).unwrap();
        assert!(!unknown.has_window() && !unknown.definitely_closed());
    }

    #[test]
    fn sanitize_strips_controls_bidi_and_truncates() {
        let s = sanitize("user\u{1b}[31m'\u{202E}evil\nline\u{7f}", 128);
        assert!(!s.chars().any(|c| c.is_control() || c == '\u{202E}'));
        assert_eq!(
            sanitize(&"é".repeat(300), EVENT_TEXT_MAX_CHARS)
                .chars()
                .count(),
            EVENT_TEXT_MAX_CHARS
        );
        assert!(valid_cursor("s=abc;i=1"));
        assert!(!valid_cursor("has space"));
        assert!(!valid_cursor(""));
        assert!(!valid_cursor(&"a".repeat(MAX_CURSOR_LEN + 1)));
    }
}
