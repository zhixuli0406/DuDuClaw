//! Wire protocol for `duduclaw-sysd`'s Unix domain socket.
//!
//! Transport: one JSON object per line (newline-delimited), one request
//! per connection — the client writes exactly one [`SysdRequest`] line,
//! reads exactly one [`SysdResponse`] line, then closes. Call volume here
//! is human-triggered (reboot/update/factory-reset), so there is no need
//! for the connection-pooling / multiplexing machinery the former `duduclaw-cli-worker`
//! HTTP protocol uses.
//!
//! [`SysdRequest`] is a **closed enum**
//! (`#[serde(tag = "verb", content = "params", deny_unknown_fields)]` —
//! the same adjacently-tagged shape that crate's protocol used)
//! — the entire caller-reachable surface is twenty fixed verbs, wire-encoded
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
//! - [`SysdRequest::SshMaintOpen`] `{ public_key, ttl_seconds, window_id }`
//!   never lets caller text near argv or a path: `window_id` passes a
//!   `[A-Za-z0-9-]{1,64}` whitelist and is only recorded, `ttl_seconds` is a
//!   bounded number, and `public_key` is decoded and checked field by field
//!   by `ssh_key::parse` (one white-listed key type, exact sizes, minimal
//!   integers); the file sysd writes holds fixed options plus the canonical
//!   base64 of the decoded bytes, never the caller's string.
//!
//! - [`SysdRequest::LocalModelService`] `{ action }` and
//!   [`SysdRequest::FactoryResetV2`] `{ clear_network }` use the
//!   [`SysdRequest::SetNtp`] discipline (a bool or a closed enum selects a
//!   literal). For `LocalModelService`: `action` is a closed
//!   [`LocalModelAction`] enum (`stop` / `restart`; anything else fails to
//!   parse), each variant selects one `&'static str` systemctl verb, and the
//!   unit is the fixed literal [`LOCAL_MODEL_UNIT`]. No caller text reaches
//!   argv.
//!
//! An unrecognized `verb` string, or any malformed JSON, fails
//! `serde_json::from_str` and the server responds with a structured
//! `bad_request` error — never a panic, never a silent no-op.
//!
//! ## Who the caller is (threat model)
//!
//! The server accepts exactly one peer uid. Once the OS runs the gateway
//! unprivileged and points `--allowed-uid` at it, that uid is the same one
//! the AI employees' CLIs run as, so a uid check alone admits any agent
//! process, which can open the socket directly and bypass every gateway-side
//! approval. Two consequences:
//!
//! 1. **The caller must be authenticated as the gateway process itself.**
//!    `--require-main-pid-of <unit>` (see `server.rs`) additionally requires
//!    the peer to be that unit's main process, proven through a pidfd so a
//!    pid reused after a gateway restart cannot pass: the peer's pidfd
//!    (`SO_PEERPIDFD`, Linux 6.5+), the pid it pins, the unit's `MainPID`
//!    read afterwards, the process's own cgroup being the unit's, and the
//!    pidfd still pinning that live process at the end. At most four
//!    verifications run at once and the `systemctl` lookup is bounded to
//!    two seconds; anything that fails or is unavailable denies, and on
//!    non-Linux hosts the flag denies every call. It
//!    is opt-in so older OS images keep working, and the OS must turn it on
//!    together with the non-root gateway. It holds only while (a) yama
//!    `kernel.yama.ptrace_scope >= 1`, so the agent processes, which are the
//!    gateway's descendants and share its uid, cannot ptrace the gateway and
//!    speak with its pid, and (b) the gateway never hands a connected sysd
//!    socket to a child: the client opens one connection per call and drops
//!    it when the call returns, and Rust std / tokio create every socket with
//!    `SOCK_CLOEXEC`, so a spawned CLI does not inherit it. The `os_*` MCP
//!    tools run in a separate `duduclaw mcp-server` process and therefore
//!    reach sysd through the gateway's signed loopback route
//!    (`duduclaw_gateway::sysd_route`, a closed verb subset), never this
//!    socket.
//! 2. **Every request is still treated as attacker-controlled.** The parameterised verbs are built for that: closed
//! enums or bools for choices, length and character whitelists for strings
//! (`Hostname`, `SetTimezone`, `NetworkWiredConfig`'s `interface`,
//! `ClearExhaustedUpdateTarget`), typed re-serialisation for file content,
//! fixed write targets, and no path ever joined from caller text (the one
//! file a caller string selects, the timezone, is resolved inside
//! `/usr/share/zoneinfo` and must be a regular file there).
//!
//! The verbs without a free-form value (the fieldless ones, plus
//! `LocalModelService` and `FactoryResetV2`, whose single field selects a
//! literal) carry nothing to inject, but a caller that passes the gates can
//! still trigger them, and two of them are not mere availability:
//! [`SysdRequest::UpdateRollback`] boots the previous A/B slot, which after
//! the first non-root upgrade is the old image whose gateway (and so the
//! agents) still ran as root; [`SysdRequest::SshServiceStart`] opens a
//! network service. The others (reboot, power off, update apply, factory
//! reset, clearing Wi-Fi, stopping the model service) cost availability or
//! data, not privilege — except [`SysdRequest::SysupdateApply`] on an image
//! whose sysupdate source is the gateway's writable download directory (see
//! that verb); [`SysdRequest::SysupdateStageApply`] is the verb that keeps
//! the trust decision on the root side. The main-pid gate above keeps an agent from calling
//! sysd directly, but it does not make the agent harmless: an AI employee
//! with a shell runs as the gateway's uid, can read the gateway's keys and
//! so forge any request the gateway makes (its signed internal routes, a
//! dashboard session). The non-root gateway keeps such an agent from root,
//! not from the gateway's own powers; the rule that real containment means
//! not granting Bash, or using the task sandbox, still applies.
//!
//! Therefore the guard against a root-regaining rollback does not look at
//! the caller at all: `--min-rollback-version` (`rollback_floor.rs`) makes
//! `UpdateRollback` refuse when any entry it could boot is older than the
//! first release with an unprivileged gateway, or its version cannot be read
//! from the entry's own unified kernel image. `SysupdateStageApply` applies
//! the same floor, and requires a strictly newer version, to what it
//! installs, reading the version from the staged UKI after verifying the
//! release signature itself; `ClearExhaustedUpdateTarget` only deletes an
//! exhausted, non-running entry. Residual, outside sysd:
//! sd-boot's own boot counting falls back to the other entry when a new one
//! fails to start three times before it is blessed, and boots an exhausted
//! entry when nothing else works; the OS should delete entries below the
//! floor once a rootless release has booted successfully.

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

/// Maximum accepted length of a `Hostname { set }` value: 64, the limit
/// `systemd-hostnamed` enforces for the static host name (`HOST_NAME_MAX`),
/// so a value that passes here is one `hostnamectl` can actually set.
pub const MAX_HOSTNAME_LEN: usize = 64;

/// Maximum accepted length (bytes) of a `SetTimezone { timezone }` value.
/// IANA tz database identifiers (e.g. `"America/Argentina/Buenos_Aires"`,
/// 30 bytes) are comfortably under this; 64 leaves headroom without
/// admitting an obviously-wrong multi-kilobyte payload before the
/// zoneinfo whitelist check in `dispatch.rs` even runs.
pub const MAX_TIMEZONE_LEN: usize = 64;

/// An SSH server unit the maintenance-mode verbs may act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SshUnit {
    pub unit: &'static str,
    /// Unit pattern of its per-connection sessions, if it has any.
    pub sessions: Option<&'static str>,
    /// cgroup v2 directory holding every session process. Closing kills
    /// everything under it and then checks it is empty, because upstream
    /// `dropbear@.service` sets `KillMode=process`: stopping a session unit
    /// ends only the dropbear process, and a shell's background jobs, `nohup`
    /// or tmux stay behind in that cgroup.
    pub session_cgroup: Option<&'static str>,
}

/// SSH server units in preference order. Opening starts the first one
/// systemd reports as loaded; closing handles every loaded one. A closed
/// list of literals, so a caller can never name an arbitrary unit.
///
/// The Yocto line ships dropbear socket-activated (`dropbear.socket`,
/// `Accept=yes`, one `dropbear@<n>.service` per connection, all under
/// `system-dropbear.slice` — openembedded-core `meta/recipes-core/dropbear`
/// at the commit `kas/duduclaw-os.yml` pins). The frozen Debian line ships
/// OpenSSH as `ssh.service`, whose sessions live in logind session scopes
/// and are not tracked here. Until 2026-10 the verbs only knew `ssh.service`,
/// which no Yocto image has, so the dashboard's maintenance SSH switch could
/// never start anything there.
pub const SSH_UNITS: [SshUnit; 2] = [
    SshUnit {
        unit: "dropbear.socket",
        sessions: Some("dropbear@*.service"),
        session_cgroup: Some("/sys/fs/cgroup/system.slice/system-dropbear.slice"),
    },
    SshUnit { unit: "ssh.service", sessions: None, session_cgroup: None },
];

/// TCP port the SSH units listen on; the verbs open and close exactly this
/// port in the OS firewall.
pub const SSH_PORT: u16 = 22;

/// The gateway's data directory on the appliance (`DUDUCLAW_HOME`, both OS
/// lines: `duduclaw-gateway.service` sets `Environment=DUDUCLAW_HOME=
/// /data/duduclaw`). [`SysdRequest::FactoryReset`] wipes its contents on the
/// root side: once the gateway runs rootless, files written through its
/// container engine can belong to subordinate uids the gateway itself cannot
/// delete. A closed constant, never a caller-supplied path.
pub const GATEWAY_HOME_DIR: &str = "/data/duduclaw";

/// Marker [`SysdRequest::FactoryResetV2`] writes to defer the wipe to the
/// next boot (see that verb for the contract).
pub const FACTORY_RESET_MARKER: &str = "/data/system/factory-reset.pending";

/// The OS oneshot that consumes [`FACTORY_RESET_MARKER`]. Its unit file
/// existing selects the deferred path.
pub const FACTORY_RESET_UNIT_FILE: &str = "/usr/lib/systemd/system/duduclaw-factory-reset.service";

/// The local model service unit ([`SysdRequest::LocalModelService`]). Must
/// match `duduclaw_inference::appliance::LLAMA_SERVER_UNIT` and the image
/// recipe.
pub const LOCAL_MODEL_UNIT: &str = "duduclaw-llama-server.service";

/// What [`SysdRequest::LocalModelService`] does to [`LOCAL_MODEL_UNIT`]. A
/// closed set: anything else fails to parse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalModelAction {
    Stop,
    Restart,
}

impl LocalModelAction {
    /// The `systemctl` verb, a literal.
    pub fn systemctl_verb(self) -> &'static str {
        match self {
            Self::Stop => "stop",
            Self::Restart => "restart",
        }
    }
}

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
    /// `systemd-sysupdate update`, installing whatever the image's
    /// transfers find in their source directory. On images whose transfers
    /// read the gateway's own download directory this is **not** merely an
    /// availability verb: sysupdate's `regular-file` source does no
    /// authentication, so a caller able to write that directory (on DuDuClaw
    /// OS, any process of the gateway's user) gets an arbitrary root image
    /// installed. Images that point their transfers at
    /// `staged_update::DEST_DIR` (root-only, empty between calls) make it
    /// harmless; use [`SysdRequest::SysupdateStageApply`] there. Refused
    /// outright when `--min-rollback-version` is set (a non-root-gateway
    /// image), and serialised with the other update verbs.
    SysupdateApply,
    /// Verify and stage an OS update as root, then install it
    /// (`staged_update.rs`): the signed manifest in the gateway's download
    /// directory is checked against the OS image key compiled into sysd, the
    /// listed payloads are hash-checked while being copied into the
    /// root-only staging directory (the UKI variant bound to the destination
    /// slot only), the staged image must be newer than the running one and
    /// at least `--min-rollback-version`, and only then does
    /// `systemd-sysupdate update` run. All checks run in a private sibling
    /// directory that is renamed into place only afterwards; the source
    /// directory is opened `O_DIRECTORY|O_NOFOLLOW` and read with `openat`.
    /// One update operation at a time (stage/apply/rollback/clear): a second
    /// is refused as `busy`. Fieldless: every path is a constant.
    SysupdateStageApply,
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
    /// Wipe the contents of the gateway's data directory
    /// ([`GATEWAY_HOME_DIR`]; a failure aborts before anything else, no
    /// reboot), then the `duduclaw-kiosk` service user's home directory
    /// (`/data/duduclaw-kiosk`, best-effort), re-arm first-boot provisioning
    /// (`systemctl enable duduclaw-firstboot-provision.service`,
    /// best-effort) then `systemctl reboot`. Deliberately carries no
    /// path/param: both directories are closed constants. Both need root:
    /// the gateway home can hold files owned by the rootless container
    /// engine's subordinate uids (until 2026-10 the gateway wiped it itself),
    /// and `/data/duduclaw-kiosk` is owned by the *different* unprivileged
    /// `duduclaw-kiosk` user (postinst.d/20-users-and-units.sh), which the
    /// sysd peer has no access to. This closes a real bug: the shell
    /// persists its OOBE-completion flag at
    /// `/data/duduclaw-kiosk/shell/oobe_state.json` (see
    /// `duduclaw-shell/src/oobe/persistence.rs`), so without this wipe a
    /// "還原原廠" left that flag set and the box booted straight back to
    /// Home instead of re-running first-time setup — the gateway-side wipe
    /// of its own home dir alone was never enough. The unit-file re-arm and
    /// reboot are unchanged: both fixed literals.
    ///
    /// Since 2026-10 this is [`SysdRequest::FactoryResetV2`] with
    /// `clear_network: false` (an older gateway clears Wi-Fi through
    /// [`SysdRequest::ClearNetworkCredentials`] first and wipes its home
    /// itself; both stay harmless when repeated).
    FactoryReset,
    /// Factory reset, choosing how by what the image ships.
    ///
    /// **Deferred (the OS ships [`FACTORY_RESET_UNIT_FILE`]).** sysd writes
    /// [`FACTORY_RESET_MARKER`] and reboots (the same `systemctl reboot` as
    /// [`SysdRequest::Reboot`]); nothing is wiped while the gateway and its
    /// containers still run, which avoided the half-wiped results the live
    /// wipe could leave (a directory refilled during the wipe, files written
    /// on shutdown). Contract with the OS oneshot:
    /// - the marker is written atomically (a temp file in `/data/system`,
    ///   then rename), mode 0600, owner root;
    /// - its content is exactly one line, `clear_network=0` or
    ///   `clear_network=1`, followed by `\n`;
    /// - a failure to write it is an error and there is no reboot;
    /// - at the next boot, before the container service and the gateway
    ///   start, the oneshot wipes the contents of `/data/duduclaw` and
    ///   `/data/duduclaw-kiosk`, removes `/data/system/containers/duduclaw`,
    ///   wipes the contents of `/data/system/iwd` when `clear_network=1`,
    ///   re-enables first-boot provisioning, and deletes the marker last.
    ///
    /// **Immediate (no such unit file: older images).** With
    /// `clear_network`, Wi-Fi credentials are cleared first
    /// ([`SysdRequest::ClearNetworkCredentials`], failure aborts); then what
    /// [`SysdRequest::FactoryReset`] always did: wipe [`GATEWAY_HOME_DIR`]
    /// (failure aborts), the kiosk home (best-effort), re-arm first boot,
    /// reboot.
    ///
    /// A separate verb so a gateway can tell an older sysd (which refuses
    /// the unknown verb and wipes nothing) from one that wipes the home.
    FactoryResetV2 { clear_network: bool },
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
    /// `commercial/docs/specs/DESIGN-network-settings-2026-08.md` §4.4).
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
    /// The legacy keyless open: start the image's SSH server ([`SSH_UNITS`]: `dropbear.socket` on the
    /// Yocto line, `ssh.service` on the Debian line) and add [`SSH_PORT`] to
    /// the OS firewall's maintenance port set when the image has a firewall.
    /// Fieldless — the unit is picked from a closed list of literals, never a
    /// caller-supplied string, so there is no way to point this verb at an
    /// arbitrary unit (see `dispatch.rs`'s module doc for the "every argv is a
    /// literal" rule this mirrors). The wire name `ssh_service_start` is kept
    /// for compatibility with older gateways.
    ///
    /// Maintenance-mode Entry A (`commercial/docs/specs/DESIGN-maintenance-mode-2026-08.md`
    /// §2.6): the ONE verb that actually changes the device's network attack
    /// surface, gated dashboard-side by Admin-only + type-to-confirm + a hard
    /// TTL. This daemon has no opinion about TTL/authorization policy at all
    /// — it only ever runs the literal command; the gateway decides when to
    /// send it.
    ///
    /// Refused as `unsupported` on an image that ships the maintenance SSH
    /// account (readiness marker `/usr/lib/duduclaw/maint-ssh.ready`, or a
    /// marker that cannot be read): there the only way in is
    /// [`SysdRequest::SshMaintOpen`]. Images without the marker keep the old
    /// behaviour. Serialised with the other SSH verbs.
    SshServiceStart,
    /// The close side of [`SysdRequest::SshServiceStart`]: stop the SSH unit,
    /// end its open sessions, and remove [`SSH_PORT`] from the firewall's
    /// maintenance port set. Idempotent: closing something that was never
    /// opened is still `success: true`, which is exactly what the
    /// maintenance-mode TTL sweep / gateway-restart reassert-closed path
    /// relies on. It reports `success: false` when a session or the port is
    /// still open afterwards, so the window's close error is not papered over.
    ///
    /// Since the maintenance SSH account (2026-10) it is also the close side
    /// of [`SysdRequest::SshMaintOpen`]: it first writes the window's pending
    /// login/logout events to the root audit file, then removes the key and
    /// `window.json`, drops sysd's deadline, cancels the PID 1 fallback timer,
    /// runs the steps above, and writes the close record. A failure in any of
    /// these is listed in `stderr` and makes `success` false. No `window_id`:
    /// there is only ever one key.
    SshServiceStop,
    /// Open a maintenance SSH window for the `duduclaw-maint` account with one
    /// public key (the `maint_ssh` module, design §8.1). In order: verify the OS is
    /// set up for it (readiness marker, account uid/gid 4800, dropbear's
    /// merged `ExecStart` token for token, `BindsTo=duduclaw-sysd.service`,
    /// the nft egress rule and jump, the port-22 source sets, the host key;
    /// any miss is `unsupported`), validate `window_id` and `ttl_seconds`
    /// (60–86400) and parse `public_key` strictly (`bad_request`, message
    /// `ssh_key:<code>`), write the key without replacing one already there
    /// (`busy`), save the window state, start sysd's own `CLOCK_BOOTTIME`
    /// deadline, arm the PID 1 fallback timer, run the
    /// [`SysdRequest::SshServiceStart`] steps and write the open record. Any
    /// failure undoes the earlier steps. Every attempt counts against a rate
    /// limit checked before anything else runs (at least 5 s apart, at most
    /// 12 per rolling hour; `rate_limited`), and an open is refused as `busy` while a previous
    /// window's audit records are still owed. On success `stdout` is one JSON line:
    /// `{"login_user","window_id","key_type","key_fingerprint","expires_in_s","host_key_fingerprints"}`.
    SshMaintOpen { public_key: String, ttl_seconds: u64, window_id: String },
    /// Read-only view of the maintenance SSH window (after writing any new
    /// journal events to the audit file): `stdout` is one JSON object with
    /// `ready`, `key_installed`, `key_fingerprint`, `window_id`,
    /// `expires_in_s`, `fallback_timer_active`, `ssh_active`, `sessions`,
    /// `events` (at most 200; logins and logouts kept first),
    /// `event_overflow`, `auth_failures`, `host_key_fingerprints`,
    /// `pending_cleanup`, `pending_audit`, `last_close` (`null` or
    /// `{"window_id": string|null, "trigger": "stop"|"deadline"|"startup_cleanup"|"reboot"|"firewall_changed"|"fallback_timer", "events_complete": bool, "events_lost": bool, "ts": unix-seconds}`),
    /// `audit_backlog_lost` (`null` or `{"ts": unix-seconds}` when such a
    /// record was written in the current boot) and `problems`.
    SshMaintStatus,
    /// `systemctl stop|restart duduclaw-llama-server.service`: the
    /// appliance's local model service, which the gateway (no longer root)
    /// cannot control itself. `action` is a closed enum; the unit is the
    /// fixed literal [`LOCAL_MODEL_UNIT`].
    LocalModelService { action: LocalModelAction },
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
            SysdRequest::SysupdateStageApply => "sysupdate_stage_apply",
            SysdRequest::BootAssessmentStatus => "boot_assessment_status",
            SysdRequest::UpdateRollback => "update_rollback",
            SysdRequest::FactoryReset => "factory_reset",
            SysdRequest::FactoryResetV2 { .. } => "factory_reset_v2",
            SysdRequest::ClearNetworkCredentials => "clear_network_credentials",
            SysdRequest::Hostname { .. } => "hostname",
            SysdRequest::SetTimezone { .. } => "set_timezone",
            SysdRequest::SetNtp { .. } => "set_ntp",
            SysdRequest::NetworkWiredConfig { .. } => "network_wired_config",
            SysdRequest::ClearExhaustedUpdateTarget { .. } => "clear_exhausted_update_target",
            SysdRequest::SshServiceStart => "ssh_service_start",
            SysdRequest::SshServiceStop => "ssh_service_stop",
            SysdRequest::SshMaintOpen { .. } => "ssh_maint_open",
            SysdRequest::SshMaintStatus => "ssh_maint_status",
            SysdRequest::LocalModelService { .. } => "local_model_service",
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
            SysdRequest::SysupdateStageApply,
            SysdRequest::SshMaintStatus,
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
    fn ssh_maint_verbs_have_the_documented_wire_shape() {
        let req = SysdRequest::SshMaintOpen {
            public_key: "ssh-ed25519 AAAA".into(),
            ttl_seconds: 14_400,
            window_id: "w-1".into(),
        };
        let s = serde_json::to_string(&req).unwrap();
        assert_eq!(
            s,
            r#"{"verb":"ssh_maint_open","params":{"public_key":"ssh-ed25519 AAAA","ttl_seconds":14400,"window_id":"w-1"}}"#
        );
        assert_eq!(serde_json::from_str::<SysdRequest>(&s).unwrap(), req);
        assert_eq!(req.verb_name(), "ssh_maint_open");
        assert_eq!(serde_json::to_string(&SysdRequest::SshMaintStatus).unwrap(), r#"{"verb":"ssh_maint_status"}"#);
        assert_eq!(SysdRequest::SshMaintStatus.verb_name(), "ssh_maint_status");
        for raw in [
            r#"{"verb":"ssh_maint_open","params":{"public_key":"k","ttl_seconds":60,"window_id":"w","comment":"x"}}"#,
            r#"{"verb":"ssh_maint_open","params":{"public_key":"k","ttl_seconds":60}}"#,
            r#"{"verb":"ssh_maint_open","params":{"public_key":"k","ttl_seconds":-1,"window_id":"w"}}"#,
            r#"{"verb":"ssh_maint_open","params":{"public_key":"k","ttl_seconds":"60","window_id":"w"}}"#,
            r#"{"verb":"ssh_maint_status","params":{}}"#,
            r#"{"verb":"ssh_maint_status","window_id":"w"}"#,
        ] {
            assert!(serde_json::from_str::<SysdRequest>(raw).is_err(), "{raw}");
        }
    }

    #[test]
    fn the_largest_ssh_maint_open_fits_in_one_request_line() {
        // An 8192-bit RSA key without its comment, the longest window id.
        let key = include_str!("../tests/fixtures/ssh_keys/rsa8192.pub");
        let mut fields = key.split_ascii_whitespace();
        let wire = format!("{} {}", fields.next().unwrap(), fields.next().unwrap());
        let req = SysdRequest::SshMaintOpen { public_key: wire, ttl_seconds: 86_400, window_id: "a".repeat(64) };
        let line = serde_json::to_string(&req).unwrap();
        assert!(line.len() < MAX_REQUEST_LINE_BYTES, "{} bytes", line.len());
        // The parser's own cap also fits, with JSON escaping of nothing (ASCII).
        let worst = SysdRequest::SshMaintOpen {
            public_key: "A".repeat(crate::ssh_key::MAX_INPUT_BYTES),
            ttl_seconds: 86_400,
            window_id: "a".repeat(64),
        };
        assert!(serde_json::to_string(&worst).unwrap().len() < MAX_REQUEST_LINE_BYTES);
    }

    #[test]
    fn local_model_service_has_a_closed_action_and_a_stable_wire_shape() {
        for (action, word) in [(LocalModelAction::Stop, "stop"), (LocalModelAction::Restart, "restart")] {
            let req = SysdRequest::LocalModelService { action };
            let s = serde_json::to_string(&req).unwrap();
            assert_eq!(s, format!(r#"{{"verb":"local_model_service","params":{{"action":"{word}"}}}}"#));
            assert_eq!(serde_json::from_str::<SysdRequest>(&s).unwrap(), req);
            assert_eq!(req.verb_name(), "local_model_service");
            assert_eq!(action.systemctl_verb(), word);
        }
        for raw in [
            r#"{"verb":"local_model_service","params":{"action":"start"}}"#,
            r#"{"verb":"local_model_service","params":{"action":"stop; reboot"}}"#,
            r#"{"verb":"local_model_service","params":{"action":"stop","unit":"sshd.service"}}"#,
            r#"{"verb":"local_model_service"}"#,
            r#"{"verb":"local_model_service","params":{"action":"Stop"}}"#,
        ] {
            assert!(serde_json::from_str::<SysdRequest>(raw).is_err(), "{raw}");
        }
        assert_eq!(LOCAL_MODEL_UNIT, "duduclaw-llama-server.service");
        assert_eq!(GATEWAY_HOME_DIR, "/data/duduclaw");
    }

    #[test]
    fn factory_reset_v2_has_a_bool_and_a_stable_wire_shape() {
        for clear_network in [true, false] {
            let req = SysdRequest::FactoryResetV2 { clear_network };
            let s = serde_json::to_string(&req).unwrap();
            assert_eq!(s, format!(r#"{{"verb":"factory_reset_v2","params":{{"clear_network":{clear_network}}}}}"#));
            assert_eq!(serde_json::from_str::<SysdRequest>(&s).unwrap(), req);
        }
        assert!(serde_json::from_str::<SysdRequest>(r#"{"verb":"factory_reset_v2","params":{"clear_network":"1"}}"#).is_err());
        assert!(serde_json::from_str::<SysdRequest>(r#"{"verb":"factory_reset_v2","params":{"clear_network":true,"path":"/"}}"#).is_err());
        assert_eq!(FACTORY_RESET_MARKER, "/data/system/factory-reset.pending");
        assert_eq!(FACTORY_RESET_UNIT_FILE, "/usr/lib/systemd/system/duduclaw-factory-reset.service");
        assert_eq!(MAX_HOSTNAME_LEN, 64);
        assert_eq!(serde_json::to_string(&SysdRequest::SysupdateStageApply).unwrap(), r#"{"verb":"sysupdate_stage_apply"}"#);
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
        // (the former duduclaw-cli-worker had the same test; that crate was
        // removed in 2026-09 with the PTY pool).
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
