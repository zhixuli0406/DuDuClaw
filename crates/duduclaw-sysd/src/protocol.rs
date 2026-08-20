//! Wire protocol for `duduclaw-sysd`'s Unix domain socket.
//!
//! Transport: one JSON object per line (newline-delimited), one request
//! per connection — the client writes exactly one [`SysdRequest`] line,
//! reads exactly one [`SysdResponse`] line, then closes. Call volume here
//! is human-triggered (reboot/update/factory-reset), so there is no need
//! for the connection-pooling / multiplexing machinery the `duduclaw-cli-worker`
//! HTTP protocol uses.
//!
//! [`SysdRequest`] is a **closed enum**
//! (`#[serde(tag = "verb", content = "params", deny_unknown_fields)]` —
//! the same adjacently-tagged shape `duduclaw-cli-worker`'s protocol uses)
//! — the entire caller-reachable surface is six fixed verbs, wire-encoded
//! as `{"verb":"reboot"}` for a fieldless verb or
//! `{"verb":"hostname","params":{"set":"..."}}` for the one verb that
//! carries data. `deny_unknown_fields` means a stray extra top-level key
//! fails to parse rather than being silently ignored. Every variant except
//! [`SysdRequest::Hostname`] carries zero fields on purpose: the server
//! never builds a command line by concatenating caller-supplied strings, it
//! only ever runs a hardcoded argv literal per verb (see `dispatch.rs`).
//! `Hostname { set }` is the one verb that carries caller data, and that
//! data is passed to the target command via `Command::arg()` (never
//! shell-interpreted), so its content cannot redirect *which* command runs
//! — only what value that one fixed command is given.
//!
//! An unrecognized `verb` string, or any malformed JSON, fails
//! `serde_json::from_str` and the server responds with a structured
//! `bad_request` error — never a panic, never a silent no-op.

use serde::{Deserialize, Serialize};

/// Default socket path baked into the appliance systemd unit
/// (`RuntimeDirectory=duduclaw` ⇒ `/run/duduclaw/`). Pinned — changing it
/// requires updating the unit file in lockstep.
pub const DEFAULT_SOCKET_PATH: &str = "/run/duduclaw/sysd.sock";

/// Env var that overrides the socket path. Read by both the server (bind)
/// and the client (connect) via [`resolve_socket_path`], so there is one
/// source of truth for "where is the socket" in dev/test environments
/// that cannot write to `/run`.
pub const SOCKET_PATH_ENV: &str = "DUDUCLAW_SYSD_SOCKET";

/// Env var carrying the single uid the server will accept requests from.
/// Absent ⇒ the server still starts (it is a resident service) but
/// [`crate::server`] denies every connection — fail-closed, not
/// fail-to-boot, matching the "no config ⇒ refuse everything" posture the
/// rest of this codebase's security gates use (see
/// `duduclaw_core::is_appliance` doc comment for the parallel).
pub const ALLOWED_UID_ENV: &str = "DUDUCLAW_SYSD_ALLOWED_UID";

/// Resolve the socket path: `$DUDUCLAW_SYSD_SOCKET` if set (and non-empty),
/// else [`DEFAULT_SOCKET_PATH`].
pub fn resolve_socket_path() -> std::path::PathBuf {
    match std::env::var(SOCKET_PATH_ENV) {
        Ok(v) if !v.trim().is_empty() => std::path::PathBuf::from(v),
        _ => std::path::PathBuf::from(DEFAULT_SOCKET_PATH),
    }
}

/// Maximum accepted length (bytes) of a single request line. Every real
/// request is well under 200 bytes (`{"verb":"hostname","set":"..."}` plus
/// a reasonable hostname); this is a defense-in-depth cap against a
/// misbehaving peer holding a connection open with an unterminated line,
/// not a limit that legitimate traffic could ever approach.
pub const MAX_REQUEST_LINE_BYTES: usize = 4096;

/// Maximum accepted length of a `Hostname { set }` value. RFC 1123 caps a
/// full hostname at 253 chars; this is intentionally generous relative to
/// that so a legitimate value is never rejected, while still refusing an
/// obviously-wrong multi-kilobyte payload before it ever reaches
/// `Command::arg()`.
pub const MAX_HOSTNAME_LEN: usize = 253;

/// The closed verb set. See module docs for the "why no free-form
/// command" reasoning.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "verb", content = "params", rename_all = "snake_case", deny_unknown_fields)]
pub enum SysdRequest {
    /// `systemctl reboot`.
    Reboot,
    /// `systemctl poweroff`.
    Poweroff,
    /// `systemd-sysupdate list --json=short`.
    SysupdateStatus,
    /// `systemd-sysupdate update`.
    SysupdateApply,
    /// Re-arm first-boot provisioning (`systemctl enable
    /// duduclaw-firstboot-provision.service`, best-effort) then
    /// `systemctl reboot`. Deliberately carries no path/param — wiping the
    /// data directory itself does not need root (the `duduclaw` user
    /// already owns it) and stays a caller-side filesystem operation; only
    /// the unit-file re-arm and reboot need root, and both are fixed
    /// literals.
    FactoryReset,
    /// `hostnamectl set-hostname <set>`. `set` is passed to
    /// `Command::arg()`, never shell-interpreted; the server still
    /// rejects empty values and values over [`MAX_HOSTNAME_LEN`] as a
    /// structured `bad_request` before spawning anything.
    Hostname { set: String },
}

impl SysdRequest {
    /// Stable short name for tracing/audit fields — cheaper than
    /// `{:?}`-formatting the whole enum (which would also print `set`'s
    /// value into logs unnecessarily for the one verb that carries data).
    pub fn verb_name(&self) -> &'static str {
        match self {
            SysdRequest::Reboot => "reboot",
            SysdRequest::Poweroff => "poweroff",
            SysdRequest::SysupdateStatus => "sysupdate_status",
            SysdRequest::SysupdateApply => "sysupdate_apply",
            SysdRequest::FactoryReset => "factory_reset",
            SysdRequest::Hostname { .. } => "hostname",
        }
    }
}

/// Successful op output — mirrors the gateway-side `device_ops::OpOutput`
/// shape (`success` is the underlying command's exit status; a non-zero
/// exit is still `ok: true` at the envelope level, since the server did
/// run the command it was asked to run).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SysdOpOutput {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

/// Structured error — `kind` is a stable token callers can branch on;
/// `message` is human-readable (zh-TW where user-facing, but this crate
/// itself stays language-neutral — the gateway layer renders copy).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SysdError {
    pub kind: String,
    pub message: String,
}

impl SysdError {
    pub fn new(kind: impl Into<String>, message: impl Into<String>) -> Self {
        Self { kind: kind.into(), message: message.into() }
    }
    pub fn unauthorized() -> Self {
        Self::new("unauthorized", "caller uid is not the configured sysd peer")
    }
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new("bad_request", message)
    }
    pub fn unsupported(message: impl Into<String>) -> Self {
        Self::new("unsupported", message)
    }
    pub fn io(message: impl Into<String>) -> Self {
        Self::new("io", message)
    }
}

/// Response envelope. Exactly one of `data`/`error` is present. Every
/// response — success, rejection, or auth denial — carries `audit_id` so
/// the caller can correlate its own log line with the server's journald
/// entry for the same call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SysdResponse {
    pub audit_id: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<SysdOpOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<SysdError>,
}

impl SysdResponse {
    pub fn ok(audit_id: impl Into<String>, data: SysdOpOutput) -> Self {
        Self { audit_id: audit_id.into(), ok: true, data: Some(data), error: None }
    }
    pub fn err(audit_id: impl Into<String>, error: SysdError) -> Self {
        Self { audit_id: audit_id.into(), ok: false, data: None, error: Some(error) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_variants_round_trip() {
        for req in [
            SysdRequest::Reboot,
            SysdRequest::Poweroff,
            SysdRequest::SysupdateStatus,
            SysdRequest::SysupdateApply,
            SysdRequest::FactoryReset,
        ] {
            let s = serde_json::to_string(&req).unwrap();
            let back: SysdRequest = serde_json::from_str(&s).unwrap();
            assert_eq!(req, back, "round-trip mismatch for {s}");
        }
    }

    #[test]
    fn hostname_variant_round_trips_with_value() {
        let req = SysdRequest::Hostname { set: "duty-box-01".to_string() };
        let s = serde_json::to_string(&req).unwrap();
        assert!(s.contains("duty-box-01"));
        let back: SysdRequest = serde_json::from_str(&s).unwrap();
        assert_eq!(req, back);
    }

    #[test]
    fn wire_shape_uses_verb_tag() {
        let s = serde_json::to_string(&SysdRequest::Reboot).unwrap();
        assert_eq!(s, r#"{"verb":"reboot"}"#);
        let s = serde_json::to_string(&SysdRequest::Hostname { set: "x".into() }).unwrap();
        assert_eq!(s, r#"{"verb":"hostname","params":{"set":"x"}}"#);
    }

    #[test]
    fn unknown_verb_fails_to_parse() {
        let raw = r#"{"verb":"rm_rf_root"}"#;
        let r: Result<SysdRequest, _> = serde_json::from_str(raw);
        assert!(r.is_err(), "unknown verb must not parse");
    }

    #[test]
    fn unknown_field_is_rejected() {
        // deny_unknown_fields — an attacker appending extra fields to a
        // valid verb must not silently pass through.
        let raw = r#"{"verb":"reboot","extra":"field"}"#;
        let r: Result<SysdRequest, _> = serde_json::from_str(raw);
        assert!(r.is_err(), "unexpected field must be rejected");
    }

    #[test]
    fn malformed_json_fails_to_parse() {
        let raw = "{not json";
        let r: Result<SysdRequest, _> = serde_json::from_str(raw);
        assert!(r.is_err());
    }

    #[test]
    fn response_ok_omits_error_and_err_omits_data() {
        let ok = SysdResponse::ok("a1", SysdOpOutput { success: true, stdout: "hi".into(), stderr: String::new() });
        let s = serde_json::to_string(&ok).unwrap();
        assert!(s.contains("\"ok\":true"));
        assert!(!s.contains("\"error\""));

        let err = SysdResponse::err("a2", SysdError::unauthorized());
        let s = serde_json::to_string(&err).unwrap();
        assert!(s.contains("\"ok\":false"));
        assert!(!s.contains("\"data\""));
    }

    #[test]
    fn resolve_socket_path_prefers_env_override() {
        // SAFETY: test-only env mutation, single-threaded within this
        // process's test but env is process-global — accept the same
        // best-effort caveat as other env-mutating tests in this workspace
        // (see duduclaw-cli-worker's `expand_home_errors_when_home_unresolvable`).
        let saved = std::env::var_os(SOCKET_PATH_ENV);
        unsafe {
            std::env::set_var(SOCKET_PATH_ENV, "/tmp/custom-sysd.sock");
        }
        assert_eq!(resolve_socket_path(), std::path::PathBuf::from("/tmp/custom-sysd.sock"));
        unsafe {
            match &saved {
                Some(v) => std::env::set_var(SOCKET_PATH_ENV, v),
                None => std::env::remove_var(SOCKET_PATH_ENV),
            }
        }
    }

    #[test]
    fn verb_name_is_stable_and_does_not_leak_hostname_value() {
        assert_eq!(SysdRequest::Reboot.verb_name(), "reboot");
        assert_eq!(
            SysdRequest::Hostname { set: "secret-ish-name".into() }.verb_name(),
            "hostname"
        );
    }
}
