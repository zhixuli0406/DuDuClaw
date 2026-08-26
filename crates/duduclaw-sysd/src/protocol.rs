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
//! — the entire caller-reachable surface is thirteen fixed verbs, wire-encoded
//! as `{"verb":"reboot"}` for a fieldless verb or
//! `{"verb":"hostname","params":{"set":"..."}}` for a verb that carries
//! data. `deny_unknown_fields` means a stray extra top-level key fails to
//! parse rather than being silently ignored.
//!
//! Eight variants ([`SysdRequest::Reboot`], [`SysdRequest::Poweroff`],
//! [`SysdRequest::SysupdateStatus`], [`SysdRequest::SysupdateApply`],
//! [`SysdRequest::BootAssessmentStatus`], [`SysdRequest::UpdateRollback`],
//! [`SysdRequest::FactoryReset`], [`SysdRequest::ClearNetworkCredentials`])
//! carry zero fields on purpose: for these
//! the server never builds a command line by concatenating caller-supplied
//! strings, it only ever runs a hardcoded argv literal per verb (see
//! `dispatch.rs`). The remaining five carry caller data, and each keeps
//! that data out of the argv-concatenation hazard via one of four
//! disciplines, depending on shape:
//! - [`SysdRequest::Hostname`] `{ set }` and [`SysdRequest::SetTimezone`]
//!   `{ timezone }` pass their one string straight to `Command::arg()`
//!   (never shell-interpreted, so the value can only ever be *the
//!   argument*, never *which command runs*). `Hostname` is accepted after
//!   only a length check; `SetTimezone` additionally passes a syntax check
//!   AND a whitelist containment check against the real
//!   `/usr/share/zoneinfo` database (`dispatch::validate_timezone_syntax` /
//!   `dispatch::timezone_exists`) before ever reaching `timedatectl
//!   set-timezone` — a directory-traversal payload there is a risk
//!   `Command::arg()`'s injection-safety alone does not close (it stops
//!   shell interpretation, not a value like `../../etc/passwd` reaching the
//!   argument itself).
//! - [`SysdRequest::SetNtp`] `{ enabled }` never lets caller text near argv
//!   at all: the bool selects between two `&'static str` literals
//!   (`"true"` / `"false"`).
//! - [`SysdRequest::NetworkWiredConfig`] is the one verb with a real
//!   multi-field payload, and it uses a third discipline: every field is
//!   parsed into a typed Rust value first (`std::net::Ipv4Addr`, a prefix
//!   `u8`, a closed `WiredMode` enum, …), and the `.network` file the
//!   server writes is *regenerated* from those typed values
//!   (`dispatch::render_wired_network`) rather than ever having a
//!   caller-supplied string written to disk verbatim — so there is no
//!   string to escape or sanitize in the first place, only typed values to
//!   re-serialize.
//! - [`SysdRequest::ClearExhaustedUpdateTarget`] `{ version }` uses a
//!   fourth discipline: `version` never reaches a spawned process, and it
//!   never reaches a path join either. It is validated into a typed value
//!   first (`dispatch::validate_update_version_syntax`, the identical
//!   character class `os_update::is_version_text` enforces gateway-side),
//!   then used only to compute a boot-entry *stem* that is compared against
//!   filenames this process already enumerated from a real directory
//!   listing (`dispatch::read_esp_entries`) — the one filesystem write this
//!   verb performs ever only targets a filename it just read back out of
//!   that same listing, never a path built by joining caller text onto a
//!   base directory (see `dispatch::dispatch_clear_exhausted_update_target`
//!   for the H3d §11.7 bug this closes).
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

/// Maximum accepted length (bytes) of a `SetTimezone { timezone }` value.
/// IANA tz database identifiers (e.g. `"America/Argentina/Buenos_Aires"`,
/// 30 bytes) are comfortably under this; 64 leaves headroom without
/// admitting an obviously-wrong multi-kilobyte payload before the
/// zoneinfo whitelist check in `dispatch.rs` even runs.
pub const MAX_TIMEZONE_LEN: usize = 64;

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
    /// `/usr/lib/systemd/systemd-bless-boot status` — reports
    /// `good` / `bad` / `indeterminate` / `clean`. Read-only: it never
    /// renames anything and never reboots, so the dashboard can ask "is a
    /// rollback possible right now" without side effects.
    ///
    /// The distinction that matters: `clean` means this boot is **not**
    /// being counted (either the entry was already blessed, or — the
    /// dangerous case — boot counting is silently a no-op because the ESP
    /// is unwritable). `indeterminate` means a counter is in flight.
    BootAssessmentStatus,
    /// Roll back to the previously-installed A/B slot, then reboot.
    ///
    /// Carries **no slot parameter on purpose**. This is a *relative*
    /// operation ("not the one I am running"), never an absolute one
    /// ("switch to slot N"): there is no slot arithmetic to get wrong, which
    /// is the whole reason `device_ops::update_rollback` refused to exist
    /// until this verb did. See `dispatch::dispatch_update_rollback` for the
    /// two tiers and why the second one is needed.
    UpdateRollback,
    /// Wipe the `duduclaw-kiosk` service user's home directory
    /// (`/data/duduclaw-kiosk`, best-effort), re-arm first-boot provisioning
    /// (`systemctl enable duduclaw-firstboot-provision.service`,
    /// best-effort) then `systemctl reboot`. Deliberately carries no
    /// path/param — wiping the GATEWAY's data directory does not need root
    /// (the `duduclaw` user already owns it) and stays a caller-side
    /// filesystem operation, but wiping `/data/duduclaw-kiosk` DOES need
    /// root: it is owned by the *different* unprivileged `duduclaw-kiosk`
    /// user (postinst.d/20-users-and-units.sh), which is the sysd peer
    /// (`duduclaw`) has no access to. This closes a real bug: the shell
    /// persists its OOBE-completion flag at
    /// `/data/duduclaw-kiosk/shell/oobe_state.json` (see
    /// `duduclaw-shell/src/oobe/persistence.rs`), so without this wipe a
    /// "還原原廠" left that flag set and the box booted straight back to
    /// Home instead of re-running first-time setup — the gateway-side wipe
    /// of its own home dir alone was never enough. The unit-file re-arm and
    /// reboot are unchanged: both fixed literals.
    FactoryReset,
    /// Wipe the CONTENTS of `/data/network/iwd` — the iwd-managed store of
    /// saved Wi-Fi credentials (0700 root:root, see
    /// `appliance/mkosi.extra/usr/lib/tmpfiles.d/duduclaw-network.conf`).
    /// The directory itself is left in place (tmpfiles recreates it every
    /// boot regardless); only the credential files inside are removed.
    /// Needs root because the `duduclaw` gateway user has no access to a
    /// root-owned directory. Carries no param: it is an unconditional wipe
    /// of a single fixed, well-known path, never a caller-shaped one.
    /// Dispatched only when an operator opts in to "一併清除網路設定" on
    /// the factory-reset confirmation — the default is to KEEP saved
    /// Wi-Fi credentials, since losing them on a headless LAN appliance can
    /// mean permanent physical-access-only recovery (see
    /// `commercial/docs/DESIGN-network-settings-2026-08.md` §4.4).
    ClearNetworkCredentials,
    /// `hostnamectl set-hostname <set>`. `set` is passed to
    /// `Command::arg()`, never shell-interpreted; the server still
    /// rejects empty values and values over [`MAX_HOSTNAME_LEN`] as a
    /// structured `bad_request` before spawning anything.
    Hostname { set: String },
    /// `timedatectl set-timezone <timezone>`. `timezone` is passed to
    /// `Command::arg()`, never shell-interpreted, but ALSO gated by a
    /// syntax check plus a whitelist containment check against the real
    /// `/usr/share/zoneinfo` database before ever reaching that argv slot
    /// — see `dispatch::validate_timezone_syntax` and
    /// `dispatch::timezone_exists`. Rejected as `bad_request` on any
    /// syntax/whitelist failure; if the zoneinfo database itself is
    /// missing on this host, rejected as `unsupported` rather than
    /// silently skipping the whitelist.
    SetTimezone { timezone: String },
    /// `timedatectl set-ntp true` / `timedatectl set-ntp false`. `enabled`
    /// only ever selects one of two `&'static str` literals — no
    /// caller-supplied text reaches argv for this verb at all.
    SetNtp { enabled: bool },
    /// Write (or remove) the appliance's static wired-network override at
    /// `/run/systemd/network/10-duduclaw-wired.network` and ask
    /// `systemd-networkd` to pick it up. `mode == "dhcp"` removes any
    /// existing override file (missing file ⇒ success, not an error);
    /// `mode == "static"` requires `address` and validates every field
    /// into a typed value (`std::net::Ipv4Addr` + prefix, `IpAddr` for
    /// `dns`) before the `.network` file content is *regenerated* from
    /// those typed values — no caller string is ever written to disk
    /// verbatim. `address`/`gateway`/`dns` are meaningful only when
    /// `mode == "static"`; IPv6 is not supported yet for `address` /
    /// `gateway` (a distinct, honestly-labeled `bad_request`, not lumped
    /// in with "not a valid address"). See `dispatch::render_wired_network`
    /// for the exact file shape.
    NetworkWiredConfig {
        interface: String,
        mode: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        address: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        gateway: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        dns: Vec<String>,
    },
    /// H3d §11.7: clear a stale, **exhausted** ESP boot entry for `version`
    /// before installing it.
    ///
    /// Closes a real bug found in live-fire QEMU testing: once a version is
    /// staged, installed, booted and then manually rolled back
    /// (`UpdateRollback`'s tier 2), the destination partition's GPT label is
    /// left unchanged (rollback only ever touches ESP entries, never
    /// partition labels) and the ESP still holds that version's UKI,
    /// already renamed to the exhausted `+0-1` shape. Both facts are enough
    /// for `systemd-sysupdate` to count the version as already installed
    /// against its `InstancesMax=2` accounting — the root transfer matches
    /// the partition by label, the UKI transfer's
    /// `duduclaw-os_@v+@l-@d.efi` pattern matches `+0-1` too — so
    /// `SysupdateApply` writes nothing at all and still exits 0. See
    /// `dispatch::dispatch_clear_exhausted_update_target` for the exact
    /// decision (idempotent no-op when there is nothing stale, and it
    /// refuses to touch the entry for the version currently running rather
    /// than ever guessing).
    ///
    /// `version` is validated by `dispatch::validate_update_version_syntax`
    /// — the same character class `os_update::is_version_text` enforces
    /// gateway-side — before it is used for anything; see the module doc
    /// comment above for why that is enough to keep this verb out of the
    /// argv/path-injection hazard the rest of this file documents.
    ClearExhaustedUpdateTarget { version: String },
    /// `systemctl start ssh.service`. Fieldless — the unit name is a fixed
    /// literal, never a caller-supplied string, so there is no way to point
    /// this verb at an arbitrary unit (see `dispatch.rs`'s module doc for the
    /// "every argv is a literal" rule this mirrors).
    ///
    /// Maintenance-mode Entry A (`commercial/docs/DESIGN-maintenance-mode-2026-08.md`
    /// §2.6): the ONE verb that actually changes the device's network attack
    /// surface, gated dashboard-side by Admin-only + type-to-confirm + a hard
    /// TTL. This daemon has no opinion about TTL/authorization policy at all
    /// — it only ever runs the literal command; the gateway decides when to
    /// send it.
    SshServiceStart,
    /// `systemctl stop ssh.service` — the close side of [`SysdRequest::SshServiceStart`].
    /// Idempotent: stopping an already-stopped unit is still `success: true`
    /// (systemd's own behavior), which is exactly what the maintenance-mode
    /// TTL sweep / gateway-restart reassert-closed path relies on — calling
    /// this when SSH was never started must never be an error.
    SshServiceStop,
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
            SysdRequest::BootAssessmentStatus => "boot_assessment_status",
            SysdRequest::UpdateRollback => "update_rollback",
            SysdRequest::FactoryReset => "factory_reset",
            SysdRequest::ClearNetworkCredentials => "clear_network_credentials",
            SysdRequest::Hostname { .. } => "hostname",
            SysdRequest::SetTimezone { .. } => "set_timezone",
            SysdRequest::SetNtp { .. } => "set_ntp",
            SysdRequest::NetworkWiredConfig { .. } => "network_wired_config",
            SysdRequest::ClearExhaustedUpdateTarget { .. } => "clear_exhausted_update_target",
            SysdRequest::SshServiceStart => "ssh_service_start",
            SysdRequest::SshServiceStop => "ssh_service_stop",
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
            SysdRequest::BootAssessmentStatus,
            SysdRequest::UpdateRollback,
            SysdRequest::FactoryReset,
            SysdRequest::ClearNetworkCredentials,
            SysdRequest::SshServiceStart,
            SysdRequest::SshServiceStop,
        ] {
            let s = serde_json::to_string(&req).unwrap();
            let back: SysdRequest = serde_json::from_str(&s).unwrap();
            assert_eq!(req, back, "round-trip mismatch for {s}");
        }
    }

    #[test]
    fn ssh_service_verbs_have_the_documented_stable_wire_shape() {
        let s = serde_json::to_string(&SysdRequest::SshServiceStart).unwrap();
        assert_eq!(s, r#"{"verb":"ssh_service_start"}"#);
        assert_eq!(SysdRequest::SshServiceStart.verb_name(), "ssh_service_start");

        let s = serde_json::to_string(&SysdRequest::SshServiceStop).unwrap();
        assert_eq!(s, r#"{"verb":"ssh_service_stop"}"#);
        assert_eq!(SysdRequest::SshServiceStop.verb_name(), "ssh_service_stop");
    }

    #[test]
    fn clear_network_credentials_has_the_documented_stable_wire_shape() {
        let s = serde_json::to_string(&SysdRequest::ClearNetworkCredentials).unwrap();
        assert_eq!(s, r#"{"verb":"clear_network_credentials"}"#);
        assert_eq!(SysdRequest::ClearNetworkCredentials.verb_name(), "clear_network_credentials");
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

    #[test]
    fn set_timezone_round_trips_with_exact_wire_shape() {
        let req = SysdRequest::SetTimezone { timezone: "Asia/Taipei".to_string() };
        let s = serde_json::to_string(&req).unwrap();
        assert_eq!(s, r#"{"verb":"set_timezone","params":{"timezone":"Asia/Taipei"}}"#);
        let back: SysdRequest = serde_json::from_str(&s).unwrap();
        assert_eq!(req, back);
    }

    #[test]
    fn set_ntp_round_trips_both_bool_values_with_exact_wire_shape() {
        for enabled in [true, false] {
            let req = SysdRequest::SetNtp { enabled };
            let s = serde_json::to_string(&req).unwrap();
            assert_eq!(s, format!(r#"{{"verb":"set_ntp","params":{{"enabled":{enabled}}}}}"#));
            let back: SysdRequest = serde_json::from_str(&s).unwrap();
            assert_eq!(req, back);
        }
    }

    #[test]
    fn network_wired_config_dhcp_omits_static_only_fields_on_the_wire() {
        let req = SysdRequest::NetworkWiredConfig {
            interface: "enp1s0".to_string(),
            mode: "dhcp".to_string(),
            address: None,
            gateway: None,
            dns: Vec::new(),
        };
        let s = serde_json::to_string(&req).unwrap();
        assert_eq!(
            s,
            r#"{"verb":"network_wired_config","params":{"interface":"enp1s0","mode":"dhcp"}}"#
        );
        let back: SysdRequest = serde_json::from_str(&s).unwrap();
        assert_eq!(req, back);
    }

    #[test]
    fn network_wired_config_static_round_trips_with_exact_wire_shape() {
        let req = SysdRequest::NetworkWiredConfig {
            interface: "enp1s0".to_string(),
            mode: "static".to_string(),
            address: Some("192.168.1.50/24".to_string()),
            gateway: Some("192.168.1.1".to_string()),
            dns: vec!["192.168.1.1".to_string(), "1.1.1.1".to_string()],
        };
        let s = serde_json::to_string(&req).unwrap();
        assert_eq!(
            s,
            r#"{"verb":"network_wired_config","params":{"interface":"enp1s0","mode":"static","address":"192.168.1.50/24","gateway":"192.168.1.1","dns":["192.168.1.1","1.1.1.1"]}}"#
        );
        let back: SysdRequest = serde_json::from_str(&s).unwrap();
        assert_eq!(req, back);
    }

    #[test]
    fn network_wired_config_parses_without_optional_fields_present_on_the_wire() {
        // A dhcp request need not send address/gateway/dns at all —
        // `#[serde(default)]` makes them optional to RECEIVE, not just
        // optional to emit.
        let raw = r#"{"verb":"network_wired_config","params":{"interface":"enp1s0","mode":"dhcp"}}"#;
        let req: SysdRequest = serde_json::from_str(raw).unwrap();
        assert_eq!(
            req,
            SysdRequest::NetworkWiredConfig {
                interface: "enp1s0".to_string(),
                mode: "dhcp".to_string(),
                address: None,
                gateway: None,
                dns: Vec::new(),
            }
        );
    }

    #[test]
    fn new_verbs_have_the_documented_stable_names() {
        assert_eq!(
            SysdRequest::SetTimezone { timezone: "Asia/Taipei".into() }.verb_name(),
            "set_timezone"
        );
        assert_eq!(SysdRequest::SetNtp { enabled: true }.verb_name(), "set_ntp");
        assert_eq!(
            SysdRequest::NetworkWiredConfig {
                interface: "enp1s0".into(),
                mode: "dhcp".into(),
                address: None,
                gateway: None,
                dns: Vec::new(),
            }
            .verb_name(),
            "network_wired_config"
        );
    }

    #[test]
    fn network_wired_config_rejects_unknown_param_field() {
        // Mirrors `unknown_field_is_rejected` but for a struct-variant's
        // OWN fields, not just the top-level {verb, params} envelope —
        // confirms `deny_unknown_fields` reaches into a verb's params.
        let raw = r#"{"verb":"network_wired_config","params":{"interface":"enp1s0","mode":"dhcp","extra":"x"}}"#;
        let r: Result<SysdRequest, _> = serde_json::from_str(raw);
        assert!(r.is_err(), "unexpected param field must be rejected");
    }

    #[test]
    fn set_timezone_rejects_unknown_param_field() {
        let raw = r#"{"verb":"set_timezone","params":{"timezone":"Asia/Taipei","extra":"x"}}"#;
        let r: Result<SysdRequest, _> = serde_json::from_str(raw);
        assert!(r.is_err(), "unexpected param field must be rejected");
    }

    #[test]
    fn clear_exhausted_update_target_round_trips_with_exact_wire_shape() {
        let req = SysdRequest::ClearExhaustedUpdateTarget { version: "0.2.0".to_string() };
        let s = serde_json::to_string(&req).unwrap();
        assert_eq!(
            s,
            r#"{"verb":"clear_exhausted_update_target","params":{"version":"0.2.0"}}"#
        );
        let back: SysdRequest = serde_json::from_str(&s).unwrap();
        assert_eq!(req, back);
        assert_eq!(req.verb_name(), "clear_exhausted_update_target");
    }

    #[test]
    fn clear_exhausted_update_target_rejects_unknown_param_field() {
        let raw = r#"{"verb":"clear_exhausted_update_target","params":{"version":"0.2.0","extra":"x"}}"#;
        let r: Result<SysdRequest, _> = serde_json::from_str(raw);
        assert!(r.is_err(), "unexpected param field must be rejected");
    }
}
