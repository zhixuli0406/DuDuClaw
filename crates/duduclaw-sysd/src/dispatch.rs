//! Verb → hardcoded shell-out dispatch.
//!
//! Every `Command::new(...)` argument here is a literal, exactly the
//! discipline `duduclaw-gateway/src/device_ops.rs` documents for its own
//! `SystemDeviceOps` — this module is that same rule applied on the root
//! side of the privilege boundary. Caller-supplied values that reach a
//! spawned process (`Hostname`'s `set`, `SetTimezone`'s `timezone`,
//! `NetworkWiredConfig`'s `interface`) pass a character whitelist that
//! forbids a leading `-` and are placed after a literal `--`, via
//! `Command::arg()` (never a shell): their content can only ever be *the
//! value*, never *which command runs* nor *an option* (`hostnamectl -H
//! <host>` would run ssh as root). The caller is treated as hostile: see
//! the threat-model section of `protocol.rs`.

use std::net::{IpAddr, Ipv4Addr};
use std::path::Path;

use tokio::process::Command;

use crate::protocol::{
    FACTORY_RESET_MARKER, FACTORY_RESET_UNIT_FILE, GATEWAY_HOME_DIR, LOCAL_MODEL_UNIT, LocalModelAction, MAX_HOSTNAME_LEN, MAX_TIMEZONE_LEN, SSH_PORT,
    SSH_UNITS, SysdError, SysdOpOutput, SysdRequest,
};

/// Absolute path to `systemd-sysupdate`.
///
/// **Not** a bare `systemd-sysupdate`: Debian ships this binary in
/// `systemd-container` under `/usr/lib/systemd/`, which is on no service's
/// `PATH`. Measured inside the appliance VM (2026-08-23, H3a acceptance
/// probe): `command -v systemd-sysupdate` → nothing, while
/// `test -x /usr/lib/systemd/systemd-sysupdate` → present. Spawning it by
/// name therefore failed with ENOENT on the one platform these verbs exist
/// for, which the caller only ever saw as a generic "failed to spawn".
///
/// `/usr/lib/` (not `/lib/`) is the real location on a merged-`/usr` trixie
/// image; `/lib` is a compatibility symlink there, so this path resolves on
/// both spellings while naming the canonical one.
const SYSTEMD_SYSUPDATE_BIN: &str = "/usr/lib/systemd/systemd-sysupdate";

/// Absolute path to `systemd-bless-boot` (Debian package `systemd-boot`),
/// which lives in the same not-on-`PATH` directory as
/// [`SYSTEMD_SYSUPDATE_BIN`]. Dispatched by the `BootAssessmentStatus` and
/// `UpdateRollback` verbs (H3f).
const SYSTEMD_BLESS_BOOT_BIN: &str = "/usr/lib/systemd/systemd-bless-boot";

/// EFI vendor GUID systemd's boot-loader interface uses for every
/// `Loader*` variable.
const LOADER_VENDOR_GUID: &str = "4a67b082-0a4c-41cf-b6c7-440b29bb8c4f";

/// efivarfs mount point. Every `Loader*` variable is world-readable here on
/// a normal EFI boot; the whole tree is simply absent in a container, on a
/// legacy-BIOS boot, and on a dev machine — all of which must degrade to an
/// honest refusal rather than a guess.
const EFIVARS_DIR: &str = "/sys/firmware/efi/efivars";

/// Directory holding Type#2 unified kernel images inside the ESP. This
/// image ships no Type#1 `loader/entries/`, so an entry id *is* a filename
/// in here (see appliance/README.md's A/B section).
const ESP_ENTRIES_SUBDIR: &str = "EFI/Linux";

/// Absolute root of the system tz database. Debian (and effectively every
/// Linux distro) ships tzdata here; this is also where [`timezone_exists`]
/// whitelists a `SetTimezone { timezone }` value against.
const ZONEINFO_ROOT: &str = "/usr/share/zoneinfo";

/// Root of the sysfs directory whose entries are exactly the interfaces
/// currently known to the kernel. Real production value passed to
/// [`interface_exists`]; tests pass a temp dir instead (this dev Mac has
/// no `/sys/class/net` at all).
const NET_CLASS_ROOT: &str = "/sys/class/net";

/// `IFNAMSIZ - 1` (`net/if.h`) — the kernel's own hard cap on an
/// interface name's length.
const MAX_INTERFACE_LEN: usize = 15;

/// Where a `NetworkWiredConfig` static override is written. `/run` is
/// tmpfs, so this keeps working under a future read-only root. `10-`
/// deliberately sorts before the shipped `20-wired-dhcp.network`
/// (systemd.network(5): the first `.network` file matching an interface,
/// in filename sort order, is the one that applies to it), so this file
/// wins for the interface it names without the shipped DHCP file ever
/// being touched — switching a NIC back to `dhcp` just means removing
/// this override, not rewriting the base file.
const WIRED_NETWORK_DIR: &str = "/run/systemd/network";

/// Filename of the static override inside [`WIRED_NETWORK_DIR`].
const WIRED_NETWORK_FILENAME: &str = "10-duduclaw-wired.network";

/// Maximum accepted `dns` entries for a static `NetworkWiredConfig`.
const MAX_DNS_ENTRIES: usize = 3;

/// Home directory of the `duduclaw-kiosk` service user (`useradd -d
/// /data/duduclaw-kiosk`, appliance/postinst.d/20-users-and-units.sh) —
/// owned by a DIFFERENT unprivileged user than this daemon's peer
/// (`duduclaw`), so wiping it needs root. This is where the shell persists
/// its OOBE-completion flag (`shell/oobe_state.json`, see
/// `duduclaw-shell/src/oobe/persistence.rs`'s `duduclaw_home()` — the kiosk
/// session's `$DUDUCLAW_HOME` is set to exactly this path by
/// `duduclaw-kiosk-launch.sh`) plus disposable Chromium/cage browser
/// cache/profile state (postinst.d's own comment: "its entire $HOME is
/// disposable browser cache/profile state"), so wiping the WHOLE directory
/// on factory reset is both simpler and more thorough than surgically
/// deleting one file.
const KIOSK_HOME_DIR: &str = "/data/duduclaw-kiosk";

/// Directories whose CONTENTS are iwd's persisted Wi-Fi credential files,
/// one per OS line: `/data/system/iwd` on the Yocto line (DuDuClaw-OS
/// `meta-duduclaw/recipes-duduclaw/duduclaw-data-binds/files/
/// var-lib-iwd.mount`) and `/data/network/iwd` on the Debian line
/// (`appliance/mkosi.extra/usr/lib/tmpfiles.d/duduclaw-network.conf`), both
/// 0700 root:root. Until 2026-10 only the Debian path was listed, so on
/// Yocto images "clear network settings" deleted nothing. The directories
/// themselves are left in place (tmpfiles recreates them every boot); only
/// the credential files inside are removed, mirroring
/// `duduclaw-gateway/src/device_ops.rs`'s own `wipe_dir_contents` for
/// `home_dir` — same "the mount point survives, what's inside does not"
/// discipline this module's [`wipe_dir_contents`] below implements.
const NETWORK_CREDENTIALS_DIRS: [&str; 2] = ["/data/system/iwd", "/data/network/iwd"];

/// Remove every entry directly under `dir` (not `dir` itself). Missing
/// `dir` is not an error (nothing to wipe) — mirrors
/// `duduclaw-gateway/src/device_ops.rs`'s identically-named helper for
/// `home_dir`; this is that same discipline applied on the root side of
/// the privilege boundary, for the two paths ([`KIOSK_HOME_DIR`],
/// [`NETWORK_CREDENTIALS_DIRS`]) the unprivileged gateway process cannot
/// reach on its own.
fn wipe_dir_contents(dir: &Path) -> std::io::Result<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            std::fs::remove_dir_all(&path)?;
        } else {
            std::fs::remove_file(&path)?;
        }
    }
    Ok(())
}

pub type DispatchResult = Result<SysdOpOutput, SysdError>;

async fn run(mut cmd: Command) -> DispatchResult {
    match cmd.output().await {
        Ok(out) => Ok(SysdOpOutput {
            success: out.status.success(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }),
        Err(e) => Err(SysdError::unsupported(format!("failed to spawn: {e}"))),
    }
}

/// Wipe [`KIOSK_HOME_DIR`], `systemctl enable
/// duduclaw-firstboot-provision.service`, then `systemctl reboot`. Both the
/// wipe and the enable step are best-effort — a dev/test host with no
/// `/data/duduclaw-kiosk` (or no firstboot unit) should still complete the
/// reboot rather than abort the whole factory-reset flow; a failure in
/// either is folded into the final `stdout` as a `[warn]` line, mirroring
/// the equivalent note `SystemDeviceOps::factory_reset` used to build
/// itself before this verb existed. The kiosk-home wipe runs FIRST: it is
/// the whole point of this verb's H3g-b/M1 fix (see
/// `SysdRequest::FactoryReset`'s doc comment) and must not be silently
/// skipped just because a later step also happened to fail.
async fn dispatch_factory_reset() -> DispatchResult {
    // The gateway home first, and fail-closed: a reset that could not wipe
    // the gateway's data must not reboot into a box that still holds it
    // (the same contract the gateway-side wipe had before it moved here).
    wipe_gateway_home(Path::new(GATEWAY_HOME_DIR))
        .map_err(|e| SysdError::io(format!("wiping {GATEWAY_HOME_DIR} failed: {e}")))?;
    let kiosk_wipe_warn = match wipe_dir_contents(Path::new(KIOSK_HOME_DIR)) {
        Ok(()) => String::new(),
        Err(e) => format!("\n[warn] wiping {KIOSK_HOME_DIR} failed: {e}"),
    };

    let mut enable_cmd = Command::new("systemctl");
    enable_cmd.args(["enable", "duduclaw-firstboot-provision.service"]);
    let enable = enable_cmd.output().await;
    let enable_warn = match &enable {
        Ok(out) if out.status.success() => String::new(),
        Ok(out) => format!(
            "\n[warn] re-arming first-boot provisioning failed: {}",
            String::from_utf8_lossy(&out.stderr)
        ),
        Err(e) => format!("\n[warn] re-arming first-boot provisioning failed: {e}"),
    };

    let mut reboot_cmd = Command::new("systemctl");
    reboot_cmd.arg("reboot");
    let reboot = run(reboot_cmd).await?;
    Ok(SysdOpOutput {
        stdout: format!("{}{kiosk_wipe_warn}{enable_warn}", reboot.stdout),
        ..reboot
    })
}

/// One update-related operation at a time (`SysupdateStageApply`,
/// `SysupdateApply`, `UpdateRollback`, `ClearExhaustedUpdateTarget`): a
/// second request while one runs is refused at once, never queued, so
/// concurrent (possibly forged) calls cannot empty each other's staging
/// directory or change boot entries mid-install.
static UPDATE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn begin_update_op(lock: &'static tokio::sync::Mutex<()>) -> Result<tokio::sync::MutexGuard<'static, ()>, SysdError> {
    lock.try_lock()
        .map_err(|_| SysdError::new("busy", "another update operation is in progress; try again when it has finished"))
}

/// Plain `SysupdateApply` installs whatever the image's sysupdate source
/// holds without verification; on an image with a rollback floor (a
/// non-root gateway) only the verified `SysupdateStageApply` may install.
fn plain_sysupdate_allowed(floor: &crate::rollback_floor::RollbackFloor) -> Result<(), SysdError> {
    if *floor == crate::rollback_floor::RollbackFloor::Off {
        Ok(())
    } else {
        Err(SysdError::unsupported(
            "sysupdate_apply is disabled on this image; use sysupdate_stage_apply (verified install)",
        ))
    }
}

/// `SysupdateStageApply`: resolve the destination slot and the running
/// version, verify and stage the release as root (`staged_update::stage`,
/// blocking work on a blocking thread), install it, then empty the staging
/// directory whatever happened.
async fn dispatch_sysupdate_stage_apply() -> DispatchResult {
    use crate::staged_update::{DEST_DIR, SOURCE_DIR, reset_dir, stage};
    let dest = Path::new(DEST_DIR);
    let staged = tokio::task::spawn_blocking(|| -> Result<crate::release::ReleaseFiles, String> {
        let current = crate::release::current_root_device()?;
        let slots = crate::release::read_slot_entries(Path::new(crate::release::BY_PARTLABEL_DIR))?;
        let destination = crate::release::pick_destination_slot(&slots, &current)?;
        let dest_partuuid =
            crate::release::partuuid_of(Path::new(crate::release::BY_PARTUUID_DIR), &destination.device)?;
        stage(
            Path::new(SOURCE_DIR),
            Path::new(DEST_DIR),
            crate::release::OS_IMAGE_UPDATE_PUBKEY,
            crate::release::machine_arch(),
            &dest_partuuid,
            crate::rollback_floor::floor(),
            running_image_version().as_deref(),
        )
    })
    .await
    .map_err(|e| SysdError::io(format!("staging task failed: {e}")))
    .and_then(|r| r.map_err(|why| SysdError::unsupported(format!("update refused: {why}"))));
    let release = match staged {
        Ok(release) => release,
        Err(e) => {
            if let Err(wipe) = reset_dir(dest) {
                tracing::warn!("could not empty {DEST_DIR}: {wipe}");
            }
            return Err(e);
        }
    };
    let mut cmd = Command::new(SYSTEMD_SYSUPDATE_BIN);
    cmd.arg("update");
    let result = run(cmd).await;
    if let Err(wipe) = reset_dir(dest) {
        tracing::warn!("could not empty {DEST_DIR}: {wipe}");
    }
    let out = result?;
    Ok(SysdOpOutput { stdout: format!("verified and staged {}\n{}", release.version, out.stdout), ..out })
}

/// Which factory-reset path this image supports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResetPath {
    /// Write the marker and reboot; the OS oneshot wipes at next boot.
    Deferred,
    /// Wipe now, then reboot (images without the oneshot).
    Immediate,
}

fn reset_path(oneshot_unit_present: bool) -> ResetPath {
    if oneshot_unit_present { ResetPath::Deferred } else { ResetPath::Immediate }
}

/// The marker's exact content (see `SysdRequest::FactoryResetV2`).
fn marker_content(clear_network: bool) -> &'static str {
    if clear_network { "clear_network=1\n" } else { "clear_network=0\n" }
}

/// Write `marker` atomically: a fresh temp file next to it (mode 0600, not
/// following links), fsync, rename over the marker, fsync the directory.
fn write_reset_marker(marker: &Path, clear_network: bool) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let dir = marker.parent().ok_or_else(|| std::io::Error::other("marker has no directory"))?;
    let tmp = dir.join(".factory-reset.pending.tmp");
    match std::fs::remove_file(&tmp) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&tmp)?;
    file.write_all(marker_content(clear_network).as_bytes())?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&tmp, marker)?;
    std::fs::File::open(dir)?.sync_all()?;
    Ok(())
}

/// `FactoryResetV2` (and the legacy `FactoryReset`, as `clear_network:
/// false`): see the verb's doc comment for both paths.
async fn dispatch_factory_reset_v2(clear_network: bool) -> DispatchResult {
    match reset_path(Path::new(FACTORY_RESET_UNIT_FILE).exists()) {
        ResetPath::Deferred => {
            write_reset_marker(Path::new(FACTORY_RESET_MARKER), clear_network)
                .map_err(|e| SysdError::io(format!("writing {FACTORY_RESET_MARKER} failed: {e}")))?;
            let mut reboot_cmd = Command::new("systemctl");
            reboot_cmd.arg("reboot");
            let reboot = run(reboot_cmd).await?;
            Ok(SysdOpOutput {
                stdout: format!("factory reset scheduled for the next boot\n{}", reboot.stdout),
                ..reboot
            })
        }
        ResetPath::Immediate => {
            if clear_network {
                dispatch_clear_network_credentials().await?;
            }
            dispatch_factory_reset().await
        }
    }
}

/// Wipe the contents of the gateway home `dir` (a closed constant in
/// production, a temp dir in tests). Root runs this inside a directory the
/// unprivileged gateway owns, so the directory itself must be a real
/// directory, not a symlink planted to redirect the wipe; a missing
/// directory is nothing to wipe. Entries are never followed:
/// [`wipe_dir_contents`] removes a symlink entry as a link, and
/// `remove_dir_all` does not follow links inside the tree.
fn wipe_gateway_home(dir: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
        Ok(meta) if meta.file_type().is_symlink() || !meta.is_dir() => Err(std::io::Error::other(
            "the gateway home is not a plain directory; refusing to wipe through it",
        )),
        Ok(_) => wipe_dir_contents(dir),
    }
}

/// `systemctl <stop|restart> duduclaw-llama-server.service`: both elements
/// literals, the verb picked by the closed [`LocalModelAction`].
fn local_model_argv(action: LocalModelAction) -> [&'static str; 2] {
    [action.systemctl_verb(), LOCAL_MODEL_UNIT]
}

async fn dispatch_local_model_service(action: LocalModelAction) -> DispatchResult {
    let mut cmd = Command::new("systemctl");
    cmd.args(local_model_argv(action));
    run(cmd).await
}

/// Wipe the CONTENTS of every existing directory in
/// [`NETWORK_CREDENTIALS_DIRS`] — see that constant's doc comment for what
/// lives there and why the directory entries themselves survive. Unlike the
/// kiosk-home wipe in [`dispatch_factory_reset`], a failure here is returned
/// as a hard `Err` rather than folded into a `[warn]` note: this verb only
/// ever runs when an operator explicitly opted in to "一併清除網路設定", and
/// the gateway-side caller (`SysdDeviceOps::factory_reset`) treats that
/// opt-in as fail-closed — silently downgrading a real failure to a warning
/// would let the operator walk away believing saved Wi-Fi credentials are
/// gone when they are not. For the same reason, finding none of the
/// directories is an error, not "nothing to clear".
async fn dispatch_clear_network_credentials() -> DispatchResult {
    let dirs: Vec<&Path> = NETWORK_CREDENTIALS_DIRS.iter().map(Path::new).collect();
    match clear_credential_dirs(&dirs) {
        Ok(cleared) => Ok(SysdOpOutput {
            success: true,
            stdout: format!("cleared {}", cleared.join(", ")),
            stderr: String::new(),
        }),
        Err(e) => Err(SysdError::io(e)),
    }
}

/// Wipe the contents of each directory in `dirs` that exists and return the
/// ones cleared. Errors when one cannot be wiped or none exists.
fn clear_credential_dirs(dirs: &[&Path]) -> Result<Vec<String>, String> {
    let mut cleared = Vec::new();
    for dir in dirs {
        if !dir.is_dir() {
            continue;
        }
        wipe_dir_contents(dir).map_err(|e| format!("clearing {} failed: {e}", dir.display()))?;
        cleared.push(dir.display().to_string());
    }
    if cleared.is_empty() {
        let listed: Vec<String> = dirs.iter().map(|d| d.display().to_string()).collect();
        return Err(format!(
            "no network credential directory found (looked for {})",
            listed.join(", ")
        ));
    }
    Ok(cleared)
}

/// `hostnamectl set-hostname <name>`. Rejects an empty or over-length
/// value as a structured `bad_request` before ever spawning anything —
/// `Command::arg()` is already injection-safe regardless of content, this
/// check exists purely to refuse an obviously-wrong request early.
async fn dispatch_hostname(name: &str) -> DispatchResult {
    let host = validate_hostname_syntax(name)?;
    let mut cmd = Command::new("hostnamectl");
    cmd.args(["set-hostname", "--", host]);
    run(cmd).await
}

/// RFC 1123 host name: at most [`MAX_HOSTNAME_LEN`] bytes, dot-separated
/// labels of 1..=63 ASCII letters, digits and `-`, no label starting or
/// ending with `-`. The caller may be an agent (see `protocol.rs`), so this
/// is a whitelist: until 2026-10 only the length was checked and a value
/// such as `-Hroot@host` reached `hostnamectl` as an option (its `-H`
/// connects over ssh). The value is also placed after `--`.
pub(crate) fn validate_hostname_syntax(name: &str) -> Result<&str, SysdError> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(SysdError::bad_request("hostname value must not be empty"));
    }
    if trimmed.len() > MAX_HOSTNAME_LEN {
        return Err(SysdError::bad_request(format!(
            "hostname value exceeds {MAX_HOSTNAME_LEN} bytes"
        )));
    }
    let label_ok = |label: &str| {
        !label.is_empty()
            && label.len() <= 63
            && label.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            && !label.starts_with('-')
            && !label.ends_with('-')
    };
    if !trimmed.split('.').all(label_ok) {
        return Err(SysdError::bad_request(
            "hostname must be letters, digits and '-' in dot-separated labels (no leading or trailing '-')",
        ));
    }
    Ok(trimmed)
}

/// Pure syntax check for a `SetTimezone { timezone }` value — no
/// filesystem access, so this half is unit-testable on any host,
/// including this dev Mac (which does have `/usr/share/zoneinfo`, but the
/// point is this check must not depend on that either way). The companion
/// whitelist check against the real database is [`timezone_exists`].
pub(crate) fn validate_timezone_syntax(tz: &str) -> Result<&str, SysdError> {
    let trimmed = tz.trim();
    if trimmed.is_empty() {
        return Err(SysdError::bad_request("timezone value must not be empty"));
    }
    if trimmed.len() > MAX_TIMEZONE_LEN {
        return Err(SysdError::bad_request(format!(
            "timezone value exceeds {MAX_TIMEZONE_LEN} bytes"
        )));
    }
    if !trimmed.is_ascii() {
        return Err(SysdError::bad_request("timezone value must be ASCII"));
    }
    if !trimmed
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '_' | '.' | '/'))
    {
        return Err(SysdError::bad_request(
            "timezone value contains disallowed characters",
        ));
    }
    if trimmed.starts_with('/') || trimmed.ends_with('/') {
        return Err(SysdError::bad_request(
            "timezone value must not start or end with '/'",
        ));
    }
    if trimmed.contains("..") {
        return Err(SysdError::bad_request(
            "timezone value must not contain '..'",
        ));
    }
    if trimmed.contains("//") {
        return Err(SysdError::bad_request(
            "timezone value must not contain '//'",
        ));
    }
    if trimmed.split('/').count() > 3 {
        return Err(SysdError::bad_request(
            "timezone value has too many path segments",
        ));
    }
    // Every IANA zone segment starts with a letter or digit; requiring it
    // keeps a value from ever reading as an option (`-H…`) or a hidden name.
    if !trimmed
        .split('/')
        .all(|seg| seg.bytes().next().is_some_and(|b| b.is_ascii_alphanumeric()))
    {
        return Err(SysdError::bad_request(
            "each timezone segment must start with a letter or digit",
        ));
    }
    Ok(trimmed)
}

/// Whitelist check against the real zoneinfo database: `<root>/<tz>` must
/// canonicalize to an existing regular file that is still contained under
/// the canonicalized root — the containment check is what makes a
/// traversal payload structurally impossible (defense in depth:
/// [`validate_timezone_syntax`]'s `".."` check already rejects the obvious
/// case, but this does not rely on that alone). `root` is a parameter so
/// tests can point this at a temp dir instead of the real
/// `/usr/share/zoneinfo`.
fn timezone_exists(root: &Path, tz: &str) -> bool {
    let root_canon = match root.canonicalize() {
        Ok(p) => p,
        Err(_) => return false,
    };
    let candidate_canon = match root.join(tz).canonicalize() {
        Ok(p) => p,
        Err(_) => return false,
    };
    candidate_canon.starts_with(&root_canon) && candidate_canon.is_file()
}

/// `timedatectl set-timezone <tz>`. `tz` passes [`validate_timezone_syntax`]
/// AND the [`timezone_exists`] whitelist check before ever reaching
/// `Command::arg()` — see the module doc comment in `protocol.rs` for why
/// a plain `Command::arg()` alone is not enough for this particular value.
/// A missing zoneinfo database is a distinct `unsupported` error (fail
/// closed), never silently treated as "no whitelist to check".
async fn dispatch_set_timezone(timezone: &str) -> DispatchResult {
    let tz = validate_timezone_syntax(timezone)?;
    let root = Path::new(ZONEINFO_ROOT);
    if !root.exists() {
        return Err(SysdError::unsupported("timezone database not available"));
    }
    if !timezone_exists(root, tz) {
        return Err(SysdError::bad_request(
            "timezone is not present in the zoneinfo database",
        ));
    }
    let mut cmd = Command::new("timedatectl");
    cmd.args(["set-timezone", "--", tz]);
    run(cmd).await
}

/// Map `enabled` to one of two `&'static str` literals — zero
/// caller-supplied text ever reaches argv for the `SetNtp` verb.
fn ntp_arg(enabled: bool) -> &'static str {
    if enabled { "true" } else { "false" }
}

/// `timedatectl set-ntp true` / `timedatectl set-ntp false`.
async fn dispatch_set_ntp(enabled: bool) -> DispatchResult {
    let mut cmd = Command::new("timedatectl");
    cmd.args(["set-ntp", ntp_arg(enabled)]);
    run(cmd).await
}

/// Closed set of accepted `NetworkWiredConfig.mode` values — anything else
/// is a `bad_request`, never silently coerced to one of the two.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WiredMode {
    Dhcp,
    Static,
}

impl WiredMode {
    fn parse(raw: &str) -> Result<Self, SysdError> {
        match raw {
            "dhcp" => Ok(WiredMode::Dhcp),
            "static" => Ok(WiredMode::Static),
            other => Err(SysdError::bad_request(format!(
                "unknown network mode: {other}"
            ))),
        }
    }
}

/// Syntax-only interface name check — no filesystem access, so this half
/// is unit-testable without `/sys/class/net` existing. The companion
/// existence + containment check is [`interface_exists`].
pub(crate) fn validate_interface_syntax(iface: &str) -> Result<&str, SysdError> {
    let trimmed = iface.trim();
    if trimmed.is_empty() || trimmed.len() > MAX_INTERFACE_LEN {
        return Err(SysdError::bad_request(format!(
            "interface name must be 1..={MAX_INTERFACE_LEN} bytes"
        )));
    }
    if !trimmed
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
    {
        return Err(SysdError::bad_request(
            "interface name may only contain letters, digits, '_' and '-'",
        ));
    }
    // Never an option to `networkctl reconfigure` (also placed after `--`).
    if !trimmed.bytes().next().is_some_and(|b| b.is_ascii_alphanumeric()) {
        return Err(SysdError::bad_request(
            "interface name must start with a letter or digit",
        ));
    }
    Ok(trimmed)
}

/// Whitelist check: `<root>/<iface>` must be an entry directly in `root`.
/// `iface` already passed [`validate_interface_syntax`] (no `/`, no `.`), so
/// the join cannot leave `root`. The entry itself is not followed: in real
/// sysfs every `/sys/class/net/<iface>` is a symlink into `/sys/devices/…`,
/// so the former "canonicalized target stays under the root" check could
/// never pass on a real host and refused every interface. `root` is a
/// parameter so tests can point this at a temp dir instead of the real
/// sysfs tree.
fn interface_exists(root: &Path, iface: &str) -> bool {
    if validate_interface_syntax(iface).is_err() {
        return false;
    }
    let candidate = root.join(iface);
    candidate.parent() == Some(root) && std::fs::symlink_metadata(&candidate).is_ok()
}

/// Parse `"<ipv4>/<prefix>"` (the `NetworkWiredConfig.address` shape). An
/// IPv6 address here is a distinct, honestly-labeled `bad_request`
/// ("IPv6 is not supported yet"), never lumped in with "not a valid
/// address".
fn parse_ipv4_with_prefix(raw: &str) -> Result<(Ipv4Addr, u8), SysdError> {
    let (ip_part, prefix_part) = raw
        .split_once('/')
        .ok_or_else(|| SysdError::bad_request("address must be in <ipv4>/<prefix> form"))?;
    let ip = parse_ipv4_field(ip_part, "address")?;
    let prefix: u8 = prefix_part
        .parse()
        .map_err(|_| SysdError::bad_request("address prefix must be a number"))?;
    if !(1..=32).contains(&prefix) {
        return Err(SysdError::bad_request("address prefix must be 1..=32"));
    }
    Ok((ip, prefix))
}

/// Parse one IPv4 field (`address`'s host part, or `gateway`),
/// distinguishing "well-formed IPv6, just not supported yet" from "not an
/// address at all" so the rejection message stays honest either way.
fn parse_ipv4_field(raw: &str, field: &str) -> Result<Ipv4Addr, SysdError> {
    raw.parse::<Ipv4Addr>().map_err(|_| {
        if raw.parse::<std::net::Ipv6Addr>().is_ok() {
            SysdError::bad_request(format!("{field}: IPv6 is not supported yet"))
        } else {
            SysdError::bad_request(format!("{field} is not a valid IPv4 address"))
        }
    })
}

/// Parse and cap the `dns` list. Entries accept either address family
/// (`std::net::IpAddr`) — DNS resolver addresses are not constrained by
/// the wired link's own IPv4-only rollout this round, unlike `address` /
/// `gateway`.
fn parse_dns_entries(entries: &[String]) -> Result<Vec<IpAddr>, SysdError> {
    if entries.len() > MAX_DNS_ENTRIES {
        return Err(SysdError::bad_request(format!(
            "dns accepts at most {MAX_DNS_ENTRIES} entries"
        )));
    }
    entries
        .iter()
        .map(|e| {
            e.parse::<IpAddr>()
                .map_err(|_| SysdError::bad_request("dns entry is not a valid IP address"))
        })
        .collect()
}

/// Render the exact `.network` file content for a static wired config —
/// pure function over already-typed values (see the "regenerate, never
/// write a caller string verbatim" discipline documented in
/// `protocol.rs`). Every byte here is either a fixed literal or the
/// `Display` output of a parsed `Ipv4Addr`/`IpAddr`/`u8`, which can only
/// ever render as a legal address/prefix — never anything an attacker
/// chose. Kept strictly ASCII, matching the shipped
/// `20-wired-dhcp.network`'s own note: a non-ASCII byte anywhere in a
/// `.network` file — even in a comment — has made systemd-networkd
/// silently skip the WHOLE file.
pub(crate) fn render_wired_network(
    iface: &str,
    address: Ipv4Addr,
    prefix: u8,
    gateway: Option<Ipv4Addr>,
    dns: &[IpAddr],
) -> String {
    let mut out = String::new();
    out.push_str("[Match]\n");
    out.push_str(&format!("Name={iface}\n"));
    out.push('\n');
    out.push_str("[Network]\n");
    out.push_str("DHCP=no\n");
    out.push_str(&format!("Address={address}/{prefix}\n"));
    if let Some(gw) = gateway {
        out.push_str(&format!("Gateway={gw}\n"));
    }
    for d in dns {
        out.push_str(&format!("DNS={d}\n"));
    }
    out.push_str("IPv6AcceptRA=no\n");
    if let Some(gw) = gateway {
        out.push('\n');
        out.push_str("[Route]\n");
        out.push_str(&format!("Gateway={gw}\n"));
        out.push_str("Metric=100\n");
    }
    out
}

/// Remove the static override file if present. A missing file is success,
/// not an error — `mode == "dhcp"` means "no override", and the override
/// may never have existed in the first place. `dir` is a parameter so
/// tests can point this at a temp dir instead of the real
/// `/run/systemd/network`.
async fn remove_wired_static_config(dir: &Path) -> std::io::Result<()> {
    let path = dir.join(WIRED_NETWORK_FILENAME);
    match tokio::fs::remove_file(&path).await {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Write `content` to the static override file atomically: temp file in
/// the same directory, then `rename` — so a reader (systemd-networkd
/// re-reading on `networkctl reload`) never observes a partially-written
/// file. Mode 0644 (world-readable, not secret data — an IP config).
/// `dir` is a parameter so tests can point this at a temp dir instead of
/// the real `/run/systemd/network`, which does not exist on this dev Mac.
async fn write_wired_static_config(dir: &Path, content: &str) -> std::io::Result<()> {
    tokio::fs::create_dir_all(dir).await?;
    let final_path = dir.join(WIRED_NETWORK_FILENAME);
    let tmp_path = dir.join(format!("{WIRED_NETWORK_FILENAME}.tmp"));
    tokio::fs::write(&tmp_path, content.as_bytes()).await?;
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(&tmp_path, std::fs::Permissions::from_mode(0o644)).await?;
    }
    tokio::fs::rename(&tmp_path, &final_path).await?;
    Ok(())
}

/// `NetworkWiredConfig` — see the module doc comment in `protocol.rs` for
/// the "regenerate from typed values" security property, and this
/// module's own doc comment for the general "every argv is a literal"
/// rule this verb bends (but does not break) for its one real payload.
///
/// Validation order is deliberate: every syntax-only check (interface
/// name shape, `mode`, and — for `static` — `address`/`gateway`/`dns`
/// parsing) runs BEFORE any filesystem access, so a malformed request is
/// rejected identically on every host, including this dev Mac which has
/// no `/sys/class/net` to check interface existence against. Only after
/// all of that succeeds does the function touch the filesystem (interface
/// existence, then the config file itself) or spawn anything.
async fn dispatch_network_wired_config(
    interface: &str,
    mode: &str,
    address: Option<&str>,
    gateway: Option<&str>,
    dns: &[String],
) -> DispatchResult {
    let iface = validate_interface_syntax(interface)?;
    let wired_mode = WiredMode::parse(mode)?;

    let static_cfg = match wired_mode {
        WiredMode::Dhcp => None,
        WiredMode::Static => {
            let addr =
                address.ok_or_else(|| SysdError::bad_request("static mode requires `address`"))?;
            let (ipv4, prefix) = parse_ipv4_with_prefix(addr)?;
            let gw = gateway
                .map(|g| parse_ipv4_field(g, "gateway"))
                .transpose()?;
            let dns_ips = parse_dns_entries(dns)?;
            Some((ipv4, prefix, gw, dns_ips))
        }
    };

    let net_root = Path::new(NET_CLASS_ROOT);
    if !net_root.exists() {
        return Err(SysdError::unsupported(
            "network interface database not available",
        ));
    }
    if !interface_exists(net_root, iface) {
        return Err(SysdError::bad_request(
            "interface is not present on this host",
        ));
    }

    let write_result = match static_cfg {
        None => remove_wired_static_config(Path::new(WIRED_NETWORK_DIR)).await,
        Some((ipv4, prefix, gw, dns_ips)) => {
            let content = render_wired_network(iface, ipv4, prefix, gw, &dns_ips);
            write_wired_static_config(Path::new(WIRED_NETWORK_DIR), &content).await
        }
    };
    write_result
        .map_err(|e| SysdError::io(format!("failed to update wired network config: {e}")))?;

    // `networkctl reload` re-reads `.network` files from disk (needed
    // since we just wrote/removed one); a spawn failure here is a real
    // "could not apply this at all" and propagates like every other
    // hardcoded shell-out in this module.
    let mut reload_cmd = Command::new("networkctl");
    reload_cmd.arg("reload");
    let reload = run(reload_cmd).await?;

    // `networkctl reconfigure <iface>` reapplies the config to this one
    // interface. Its failure (including failing to spawn at all) is
    // reported in `stderr` and folded into the response, never
    // propagated as an `Err` and never a panic — the config file write
    // already succeeded by this point, and losing that success behind a
    // generic error would be misleading.
    let mut reconfigure_cmd = Command::new("networkctl");
    reconfigure_cmd.args(["reconfigure", "--", iface]);
    let (reconfigure_success, reconfigure_stdout, reconfigure_stderr) =
        match reconfigure_cmd.output().await {
            Ok(out) => (
                out.status.success(),
                String::from_utf8_lossy(&out.stdout).into_owned(),
                String::from_utf8_lossy(&out.stderr).into_owned(),
            ),
            Err(e) => (
                false,
                String::new(),
                format!("failed to spawn networkctl reconfigure: {e}"),
            ),
        };

    Ok(SysdOpOutput {
        success: reload.success && reconfigure_success,
        stdout: format!("{}\n{}", reload.stdout, reconfigure_stdout),
        stderr: format!("{}\n{}", reload.stderr, reconfigure_stderr),
    })
}

// ---------------------------------------------------------------------------
// H3f: boot assessment + manual rollback
// ---------------------------------------------------------------------------

/// The four states `systemd-bless-boot status` can report, plus an
/// `Unknown` for output this version does not recognise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlessState {
    /// A counter is in flight for this boot and has not been resolved yet.
    Indeterminate,
    /// This boot was blessed during *this* boot.
    Good,
    /// This boot's entry has already been marked bad.
    Bad,
    /// No counter is in flight. Either the entry was blessed on an earlier
    /// boot (the steady state of a healthy machine — measured in the H3b
    /// T1 probe), or boot counting is silently not running at all.
    Clean,
    Unknown,
}

/// Classify `systemd-bless-boot status` output.
///
/// Matches on whole words rather than substrings (project convention 2):
/// the word `good` must not be found inside a sentence like "no good entry".
pub fn parse_bless_status(text: &str) -> BlessState {
    let mut seen = None;
    for word in text.split(|c: char| !c.is_ascii_alphabetic()) {
        let state = match word.to_ascii_lowercase().as_str() {
            "indeterminate" => BlessState::Indeterminate,
            "good" => BlessState::Good,
            "bad" => BlessState::Bad,
            "clean" => BlessState::Clean,
            _ => continue,
        };
        // The tool prints exactly one state token; if output ever carried
        // more, the first is the verdict and later words are prose.
        if seen.is_none() {
            seen = Some(state);
        }
    }
    seen.unwrap_or(BlessState::Unknown)
}

/// Strip a boot-counting suffix from an entry filename.
///
/// `duduclaw-os_0.2.0+2-1.efi` → `("duduclaw-os_0.2.0", ".efi")`, and a
/// name with no counter comes back unchanged. Per systemd-boot(7) the
/// counter is `+` followed by one or two numbers separated by `-`, directly
/// before the suffix — anything else (a `+` inside a version, say
/// `1.0+deb13`) is not a counter and must be left alone.
pub fn split_boot_counter(name: &str) -> Option<(String, String)> {
    let stem = name.strip_suffix(".efi")?;
    let Some((base, counter)) = stem.rsplit_once('+') else {
        return Some((stem.to_string(), ".efi".to_string()));
    };
    let is_counter = match counter.split_once('-') {
        Some((left, right)) => {
            !left.is_empty()
                && !right.is_empty()
                && left.bytes().all(|b| b.is_ascii_digit())
                && right.bytes().all(|b| b.is_ascii_digit())
        }
        None => !counter.is_empty() && counter.bytes().all(|b| b.is_ascii_digit()),
    };
    if is_counter {
        Some((base.to_string(), ".efi".to_string()))
    } else {
        Some((stem.to_string(), ".efi".to_string()))
    }
}

/// True when an entry filename is already in the exhausted (`tries_left == 0`)
/// shape sd-boot sorts last.
pub fn is_exhausted_entry(name: &str) -> bool {
    let Some(stem) = name.strip_suffix(".efi") else {
        return false;
    };
    let Some((_, counter)) = stem.rsplit_once('+') else {
        return false;
    };
    let left = counter.split('-').next().unwrap_or_default();
    left == "0"
}

/// The name an entry takes once it is marked bad: `tries_left = 0`,
/// `tries_done = 1`. Exactly the shape `systemd-bless-boot bad` produces,
/// so both tiers of rollback leave the ESP in one state, not two.
pub fn exhausted_name(name: &str) -> Option<String> {
    let (base, suffix) = split_boot_counter(name)?;
    Some(format!("{base}+0-1{suffix}"))
}

/// True when `name` is a plain `.efi` filename and nothing else — no
/// separator, no traversal. Everything read out of an EFI variable goes
/// through this before it is joined onto a path.
pub fn is_entry_filename(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name.ends_with(".efi")
        && !name.starts_with('.')
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains('\0')
        && std::path::Path::new(name).file_name().and_then(|s| s.to_str()) == Some(name)
}

/// Decode an efivarfs value: 4 bytes of attributes, then a NUL-terminated
/// UTF-16LE string.
pub fn decode_efi_string(raw: &[u8]) -> Option<String> {
    let body = raw.get(4..)?;
    let units: Vec<u16> = body
        .chunks_exact(2)
        .map(|p| u16::from_le_bytes([p[0], p[1]]))
        .take_while(|u| *u != 0)
        .collect();
    let s = String::from_utf16(&units).ok()?;
    if s.is_empty() { None } else { Some(s) }
}

/// Decide whether marking `selected` bad still leaves something to boot.
///
/// **Compares by counter-stripped stem, never by literal filename.** An
/// entry's filename is the boot-assessment state machine's storage: the same
/// installed version is `duduclaw-os_0.2.0+3-0.efi` when staged,
/// `+2-1.efi` while it is being tried, and `duduclaw-os_0.2.0.efi` once
/// blessed. Measured on the appliance: on the boot that blesses a new
/// version, `LoaderBootCountPath` still names the pre-blessing
/// `…+2-1.efi` while the file on the ESP has already been renamed — so a
/// literal comparison reports "the running boot entry is not present in the
/// ESP" about a machine that is running it. The stem (`duduclaw-os_0.2.0`)
/// is the stable identity.
///
/// sd-boot's documented last-resort behaviour is that a bad entry is still
/// booted when every other entry is also bad, so refusing here cannot brick
/// a machine — but "the rollback silently did nothing useful" is still a lie
/// to the operator, and refusing honestly is the whole point of the gate.
pub fn check_rollback_target(entries: &[String], selected: &str) -> Result<(), String> {
    let want = entry_stem(selected)
        .ok_or_else(|| format!("unusable boot entry name ({selected})"))?;
    if !entries.iter().any(|e| entry_stem(e).as_deref() == Some(want.as_str())) {
        return Err(format!(
            "the running boot entry ({selected}) is not present in the ESP"
        ));
    }
    let alternatives: Vec<&String> = entries
        .iter()
        .filter(|e| {
            entry_stem(e).as_deref() != Some(want.as_str()) && !is_exhausted_entry(e)
        })
        .collect();
    if alternatives.is_empty() {
        return Err(
            "there is no other bootable version installed to fall back to".to_string(),
        );
    }
    Ok(())
}

/// The stable identity of a boot entry: its filename with any
/// boot-counting suffix and the `.efi` extension removed.
pub fn entry_stem(name: &str) -> Option<String> {
    split_boot_counter(name).map(|(stem, _)| stem)
}

async fn bless_boot(arg: &'static str) -> DispatchResult {
    let mut cmd = Command::new(SYSTEMD_BLESS_BOOT_BIN);
    cmd.arg(arg);
    run(cmd).await
}

/// Resolve the ESP mount point via `bootctl -p`, rather than hardcoding a
/// path: systemd-gpt-auto-generator mounts this image's ESP at `/boot`
/// while an empty, non-mounted `/efi` also exists — anything that guesses
/// succeeds while writing to the root filesystem.
async fn esp_entries_dir() -> Result<std::path::PathBuf, SysdError> {
    let out = Command::new("bootctl")
        .arg("-p")
        .output()
        .await
        .map_err(|e| SysdError::unsupported(format!("cannot locate the ESP: {e}")))?;
    if !out.status.success() {
        return Err(SysdError::unsupported(
            "cannot locate the ESP (bootctl -p failed)".to_string(),
        ));
    }
    let esp = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if esp.is_empty() || !esp.starts_with('/') {
        return Err(SysdError::unsupported(format!(
            "bootctl reported an unusable ESP path: {esp:?}"
        )));
    }
    let dir = std::path::Path::new(&esp).join(ESP_ENTRIES_SUBDIR);
    if !dir.is_dir() {
        return Err(SysdError::unsupported(format!(
            "{} does not exist — this image has no Type#2 boot entries",
            dir.display()
        )));
    }
    Ok(dir)
}

/// `IMAGE_VERSION=` from the running image's os-release. Same field
/// sysupdate's `ProtectVersion=%A` reads, so "which version am I" has one
/// answer across the update chain.
fn running_image_version() -> Option<String> {
    for path in ["/usr/lib/os-release", "/etc/os-release"] {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        for line in text.lines() {
            if let Some(v) = line.trim().strip_prefix("IMAGE_VERSION=") {
                let v = v.trim().trim_matches('"').trim_matches('\'');
                if !v.is_empty() {
                    return Some(v.to_string());
                }
            }
        }
    }
    None
}

/// Filename of the entry sd-boot is counting this boot, from
/// `LoaderBootCountPath`.
///
/// The variable holds an EFI-style path (`\EFI\Linux\name+2-1.efi`); only
/// the last component is of interest, and it still has to look like an entry
/// filename before it is used for anything.
pub fn boot_count_path_basename(path: &str) -> Option<String> {
    let name = path.rsplit(['\\', '/']).next()?;
    if is_entry_filename(name) {
        Some(name.to_string())
    } else {
        None
    }
}

/// Which boot entry this machine is running, as a filename.
///
/// `LoaderEntrySelected` first: sd-boot sets it on every boot and reports the
/// **stable** id (measured on the appliance: it reads
/// `duduclaw-os_0.2.0.efi` even on the boot where the file on disk was
/// `…+2-1.efi`). `LoaderBootCountPath` is the fallback — it exists only
/// while a counter is in flight and can name a filename that blessing has
/// already renamed, which is exactly why it is second and why every
/// comparison downstream goes through `entry_stem`.
fn running_entry_name() -> Option<String> {
    read_efi_loader_string("LoaderEntrySelected")
        .filter(|s| is_entry_filename(s))
        .or_else(|| {
            read_efi_loader_string("LoaderBootCountPath")
                .as_deref()
                .and_then(boot_count_path_basename)
        })
}

/// The ESP's Type#2 boot entries: the directory plus every `*.efi` filename
/// in it. One reader for both rollback tiers, so "what counts as an entry"
/// is defined in exactly one place.
async fn read_esp_entries() -> Result<(std::path::PathBuf, Vec<String>), SysdError> {
    let dir = esp_entries_dir().await?;
    let rd = std::fs::read_dir(&dir)
        .map_err(|e| SysdError::io(format!("cannot list {}: {e}", dir.display())))?;
    let mut entries: Vec<String> = Vec::new();
    for entry in rd.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if is_entry_filename(&name) {
            entries.push(name);
        }
    }
    Ok((dir, entries))
}

fn read_efi_loader_string(name: &str) -> Option<String> {
    let path = std::path::Path::new(EFIVARS_DIR).join(format!("{name}-{LOADER_VENDOR_GUID}"));
    let raw = std::fs::read(path).ok()?;
    decode_efi_string(&raw)
}

/// Roll back to the previous A/B slot, then reboot.
///
/// **Two tiers, both relative operations.**
///
/// *Tier 1 — a counter is in flight (`indeterminate`).* Hand the whole thing
/// to `systemd-bless-boot bad`, which is what the design doc specifies
/// (§4.5): it resolves "the entry I booted from" through the
/// `LoaderBootCountPath` EFI variable and sets its `tries_left` to 0. No
/// slot arithmetic, therefore nothing to get wrong.
///
/// *Tier 2 — no counter is in flight (`clean`).* Measured, and the reason
/// tier 1 alone is not enough: once a version has been blessed, its entry
/// carries no counter, `bless-boot status` answers `clean`, and
/// `bless-boot bad` has nothing to act on (the H3b T1 probe asserts exactly
/// this steady state). That is *also* the only state a user ever presses
/// "roll back" in — the new version installed, booted, was blessed, and
/// only then turned out to be wrong. So this tier reproduces the same
/// outcome by the same mechanism sd-boot itself uses: rename the entry
/// currently booted (read from `LoaderEntrySelected`, never computed) to
/// the exhausted `+0-1` shape, which sd-boot sorts last. Still relative —
/// the only entry ever touched is the one we are running from — and gated
/// on there being another, non-exhausted entry to fall back to.
///
/// Refuses (and does NOT reboot) when the machine cannot support the
/// operation at all: no EFI boot, no ESP, no second version installed. A
/// rollback that quietly does nothing but reboots anyway is worse than an
/// honest refusal.
async fn dispatch_update_rollback() -> DispatchResult {
    let status = bless_boot("status").await;
    let state = match &status {
        Ok(out) => parse_bless_status(&format!("{}{}", out.stdout, out.stderr)),
        // The binary is absent (a dev host, a container). Not an error to
        // paper over — there is no A/B machinery here at all.
        Err(e) => {
            return Err(SysdError::unsupported(format!(
                "boot assessment is unavailable on this system: {}",
                e.message
            )));
        }
    };

    // Both tiers need somewhere healthy to fall back to, and both apply the
    // same test — only the way they identify "the entry I am running"
    // differs. Tier 1 reads `LoaderBootCountPath` (set by sd-boot exactly
    // when a counter is in flight, which is exactly when tier 1 applies);
    // tier 2 reads `LoaderEntrySelected`. Neither computes a slot.
    //
    // Without this, a machine whose entries are ALL already exhausted would
    // happily "roll back" — marking a bad entry bad again and spending a
    // reboot to end up exactly where it started. Refusing is the honest
    // answer, and it is the T7 case.
    let (dir, entries) = read_esp_entries().await?;
    match running_entry_name() {
        Some(running) => {
            check_rollback_target(&entries, &running).map_err(SysdError::unsupported)?
        }
        // Nothing could tell us which entry we are. Fall back to the weaker
        // "is there more than one at all", which still catches the case that
        // matters: a single installed version, where rollback can only waste
        // a reboot.
        None if entries.len() < 2 => {
            return Err(SysdError::unsupported(
                "there is no other version installed to fall back to".to_string(),
            ));
        }
        None => {}
    }

    // Rollback floor (`--min-rollback-version`): every entry this could end
    // up booting must carry a known version at or above the floor, read from
    // the UKI itself (`rollback_floor`). Checked before any tier marks or
    // renames anything, so a refusal leaves the ESP untouched.
    let floor = crate::rollback_floor::floor();
    if *floor != crate::rollback_floor::RollbackFloor::Off {
        let running = running_entry_name();
        let versions: Vec<(&str, Result<crate::rollback_floor::OsVersion, String>)> =
            crate::rollback_floor::rollback_candidates(&entries, running.as_deref())
                .into_iter()
                .map(|name| (name.as_str(), crate::rollback_floor::entry_version(&dir, name)))
                .collect();
        crate::rollback_floor::check_floor(floor, &versions).map_err(SysdError::unsupported)?;
        // Type #1 entries cannot be verified as strictly: refuse while any exist.
        let type1 = crate::rollback_floor::esp_root_of(&dir)
            .ok_or_else(|| "the ESP root could not be determined".to_string())
            .and_then(crate::rollback_floor::type1_entries_present);
        match type1 {
            Ok(false) => {}
            Ok(true) => {
                return Err(SysdError::unsupported(
                    "refusing to roll back: the ESP holds Type #1 boot entries (loader/entries/*.conf) whose \
                     version cannot be verified"
                        .to_string(),
                ));
            }
            Err(why) => return Err(SysdError::unsupported(format!("refusing to roll back: {why}"))),
        }
    }

    let note = match state {
        BlessState::Indeterminate => {
            let marked = bless_boot("bad").await?;
            if !marked.success {
                return Err(SysdError::unsupported(format!(
                    "could not mark the running version for rollback: {}",
                    marked.stderr.trim()
                )));
            }
            "marked the in-flight boot entry bad via systemd-bless-boot".to_string()
        }
        BlessState::Bad => {
            "the running version was already marked bad; rebooting to complete the rollback"
                .to_string()
        }
        BlessState::Good | BlessState::Clean | BlessState::Unknown => {
            rollback_by_renaming_selected_entry().await?
        }
    };

    let mut reboot_cmd = Command::new("systemctl");
    reboot_cmd.arg("reboot");
    let reboot = run(reboot_cmd).await?;
    Ok(SysdOpOutput {
        stdout: format!("{note}\n{}", reboot.stdout),
        ..reboot
    })
}

/// Tier 2 of [`dispatch_update_rollback`]. Returns a human-readable note on
/// success; every failure path is a structured refusal that leaves the ESP
/// untouched.
async fn rollback_by_renaming_selected_entry() -> Result<String, SysdError> {
    // First choice: ask the boot loader which entry it started, rather than
    // working it out. `LoaderEntrySelected` is set by sd-boot on every boot
    // (unlike `LoaderBootCountPath`, which only exists while a counter is in
    // flight), and for a Type#2 image the entry id IS the filename.
    let mut selected = running_entry_name();
    let mut how = "the boot loader interface";

    // Fallback: the running image's own IMAGE_VERSION. Still not a guess at
    // *which slot* — it is this system reporting its own version, and this
    // image names every entry `duduclaw-os_<version>.efi`. Used only when
    // the firmware gave us nothing usable, and the entry it names still has
    // to exist and still has to leave a healthy alternative behind.
    if selected.is_none() {
        if let Some(v) = running_image_version() {
            selected = Some(format!("duduclaw-os_{v}.efi"));
            how = "the running IMAGE_VERSION";
        }
    }
    let selected = selected.ok_or_else(|| {
        SysdError::unsupported(
            "this system did not boot through systemd-boot, so there is no previous \
             version to return to"
                .to_string(),
        )
    })?;
    if !is_entry_filename(&selected) {
        return Err(SysdError::unsupported(format!(
            "unusable boot entry name ({selected:?})"
        )));
    }
    tracing::info!("[update_rollback] running entry resolved via {how}: {selected}");

    let (dir, entries) = read_esp_entries().await?;
    check_rollback_target(&entries, &selected).map_err(SysdError::unsupported)?;

    // Rename the file that is ACTUALLY on the ESP for this version, not the
    // name the firmware reported: the two differ whenever a counter is in
    // flight (`…+2-1.efi` on disk vs the stable id in
    // `LoaderEntrySelected`). Matching by stem is what makes both spellings
    // resolve to the one real file.
    let want = entry_stem(&selected)
        .ok_or_else(|| SysdError::unsupported("unusable boot entry name".to_string()))?;
    let on_disk = entries
        .iter()
        .find(|e| entry_stem(e).as_deref() == Some(want.as_str()))
        .ok_or_else(|| {
            SysdError::unsupported(format!("{selected} is not present in the ESP"))
        })?
        .clone();

    let target = exhausted_name(&on_disk)
        .ok_or_else(|| SysdError::unsupported("unusable boot entry name".to_string()))?;
    if target == on_disk {
        return Ok("the running version was already marked bad".to_string());
    }
    let from = dir.join(&on_disk);
    let to = dir.join(&target);
    std::fs::rename(&from, &to).map_err(|e| {
        SysdError::io(format!(
            "could not mark the running version for rollback ({}): {e}",
            from.display()
        ))
    })?;
    // FAT32 renames are not atomic and the ESP is the one thing that makes
    // this machine bootable — flush before handing over to the reboot.
    if let Ok(f) = std::fs::File::open(&dir) {
        let _ = f.sync_all();
    }
    Ok(format!("marked {on_disk} bad (renamed to {target})"))
}

// ---------------------------------------------------------------------------
// H3d §11.7: clear a stale exhausted ESP entry before reinstalling a version
// ---------------------------------------------------------------------------

/// Prefix every Type#2 boot entry filename carries — the same literal
/// `duduclaw-gateway::os_update::SLOT_LABEL_PREFIX` uses for GPT partition
/// labels. Duplicated rather than shared (this crate cannot depend on
/// `duduclaw-gateway` — the dependency direction runs the other way); kept
/// as one named constant so the two copies are trivial to eyeball against
/// each other if either ever changes.
const ENTRY_STEM_PREFIX: &str = "duduclaw-os_";

/// Maximum accepted length of a `ClearExhaustedUpdateTarget { version }`
/// value — mirrors `os_update::is_version_text`'s 32-byte cap so a version
/// this daemon accepts is exactly a version the gateway could have named in
/// a signed manifest.
const MAX_UPDATE_VERSION_LEN: usize = 32;

/// Pure syntax check for a `ClearExhaustedUpdateTarget { version }` value —
/// deliberately the *exact same* character class `os_update::is_version_text`
/// enforces gateway-side (must start with a letter or digit, then only
/// alphanumerics plus `._-+`). No filesystem access, so this half is
/// unit-testable on any host. The companion decision logic is
/// [`find_exhausted_entry`] / [`target_is_running_entry`], both pure over
/// an already-read entry list.
pub(crate) fn validate_update_version_syntax(v: &str) -> Result<&str, SysdError> {
    let trimmed = v.trim();
    if trimmed.is_empty() {
        return Err(SysdError::bad_request("version value must not be empty"));
    }
    if trimmed.len() > MAX_UPDATE_VERSION_LEN {
        return Err(SysdError::bad_request(format!(
            "version value exceeds {MAX_UPDATE_VERSION_LEN} bytes"
        )));
    }
    let starts_ok = trimmed
        .bytes()
        .next()
        .is_some_and(|b| b.is_ascii_alphanumeric());
    if !starts_ok {
        return Err(SysdError::bad_request(
            "version value must start with a letter or digit",
        ));
    }
    if !trimmed
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"._-+".contains(&b))
    {
        return Err(SysdError::bad_request(
            "version value contains disallowed characters",
        ));
    }
    Ok(trimmed)
}

/// True when `want_stem` (the stable identity of the version a caller wants
/// cleared) is the entry the machine is **currently running**. `running` is
/// whatever [`running_entry_name`] returned, if anything — compared through
/// [`entry_stem`] so a counted filename (`…+2-1.efi`) still resolves to the
/// same identity as its blessed form, exactly like [`check_rollback_target`]
/// does for the rollback path.
///
/// This is the one hard refusal in the whole verb: a caller asking to clear
/// the entry for the version the machine is presently running is either
/// confused about which version it means or attempting to make the running
/// boot entry disappear out from under the machine. Neither is this verb's
/// job — it only ever prepares a DIFFERENT, about-to-be-installed version's
/// slot.
pub fn target_is_running_entry(running: Option<&str>, want_stem: &str) -> bool {
    running.and_then(entry_stem).as_deref() == Some(want_stem)
}

/// Pure decision: which (if any) ESP entry is the stale, exhausted leftover
/// for `want_stem` that this verb needs to clear.
///
/// `None` covers two different situations on purpose, because the caller
/// (the update-apply flow) treats them identically — "nothing to do here":
/// - no entry for this version exists at all (a first install of it), or
/// - an entry exists but is **not** exhausted — still mid-assessment
///   (`+2-1`) or already blessed (no suffix). That is live boot-assessment
///   state this verb must never touch; only the exhausted `+0-…` shape is a
///   stale leftover from a rollback that already happened.
pub fn find_exhausted_entry<'a>(entries: &'a [String], want_stem: &str) -> Option<&'a String> {
    entries
        .iter()
        .find(|e| entry_stem(e).as_deref() == Some(want_stem) && is_exhausted_entry(e))
}

/// H3d §11.7: clear a stale exhausted ESP entry for `version` before
/// installing it, so `systemd-sysupdate` cannot mistake a previously
/// rolled-back version for one that is already installed.
///
/// **The bug this closes** (found in 2026-08-24 live-fire QEMU testing, not
/// in code review — see the design doc §11.7). Once a version has been
/// staged, installed, booted and then manually rolled back
/// ([`dispatch_update_rollback`]'s tier 2), the destination partition's GPT
/// label is left exactly as it was — rollback only ever renames an ESP
/// entry, it never touches a partition label — and the ESP still holds that
/// version's UKI, already renamed to the exhausted `+0-1` shape. Both of
/// those independently satisfy `systemd-sysupdate`'s `InstancesMax=2`
/// accounting for that version (the root transfer matches the partition by
/// label; the UKI transfer's `duduclaw-os_@v+@l-@d.efi` pattern matches
/// `+0-1` too), so a later `SysupdateApply` for the very same version writes
/// nothing at all and still exits 0 — "rolled back once, never installable
/// again, and the update flow lies about it."
///
/// **The fix only has to touch the ESP.** The partition's contents are
/// still correct (rollback never wrote to it), so removing the stale
/// exhausted UKI is sufficient: the next `SysupdateApply` sees the version's
/// instance count drop below `InstancesMax`, writes a fresh `+3-0` entry
/// from the (already re-verified, already PARTUUID-bound) staged UKI, and
/// sd-boot can actually try booting it again.
///
/// **Deliberately idempotent**, meant to run unconditionally before every
/// `SysupdateApply` rather than only after a detected rollback (cheaper than
/// tracking "was this version ever rolled back", and correct either way —
/// see [`find_exhausted_entry`] for the no-op cases). Refuses
/// ([`target_is_running_entry`]) rather than ever touching the entry for the
/// version currently running.
async fn dispatch_clear_exhausted_update_target(version: &str) -> DispatchResult {
    let version = validate_update_version_syntax(version)?;
    let want_stem = format!("{ENTRY_STEM_PREFIX}{version}");

    if target_is_running_entry(running_entry_name().as_deref(), &want_stem) {
        return Err(SysdError::bad_request(
            "refusing to clear the boot entry for the version currently running".to_string(),
        ));
    }

    let (dir, entries) = read_esp_entries().await?;
    let Some(target) = find_exhausted_entry(&entries, &want_stem) else {
        return Ok(SysdOpOutput {
            success: true,
            stdout: format!("no exhausted ESP entry for {version}; nothing to clear"),
            stderr: String::new(),
        });
    };

    let path = dir.join(target);
    std::fs::remove_file(&path).map_err(|e| {
        SysdError::io(format!(
            "could not remove exhausted entry {}: {e}",
            path.display()
        ))
    })?;
    // Same FAT32-is-not-atomic reasoning as the rollback rename below: the
    // ESP is what makes this machine bootable at all, flush before
    // reporting success.
    if let Ok(f) = std::fs::File::open(&dir) {
        let _ = f.sync_all();
    }
    Ok(SysdOpOutput {
        success: true,
        stdout: format!("removed exhausted ESP entry {target} for version {version}"),
        stderr: String::new(),
    })
}

// ── Maintenance-mode SSH ────────────────────────────────────

/// The OS firewall's nftables table and the named set its input chain
/// accepts TCP ports from (`tcp dport @maintenance_tcp_ports accept`,
/// DuDuClaw-OS `meta-duduclaw/recipes-duduclaw/duduclaw-firewall/files/
/// nftables.conf`, added 2026-10). The set starts empty, so nothing extra
/// is reachable until maintenance mode adds a port.
const FIREWALL_FAMILY: &str = "inet";
const FIREWALL_TABLE: &str = "filter";
const MAINTENANCE_PORT_SET: &str = "maintenance_tcp_ports";
/// The firewall's config file. When it exists the image is meant to filter,
/// so a missing table means the firewall failed to load, not that there is
/// no firewall.
const FIREWALL_CONFIG: &str = "/etc/nftables.conf";

/// What systemd says about a unit. Anything other than a clear answer is
/// `Unknown`, which callers must not read as "not installed".
#[derive(Debug, Clone, PartialEq, Eq)]
enum UnitLoad {
    Loaded,
    NotFound,
    Unknown(String),
}

fn parse_load_state(success: bool, stdout: &str, stderr: &str) -> UnitLoad {
    if !success {
        return UnitLoad::Unknown(format!("systemctl show failed: {}", stderr.trim()));
    }
    match stdout.trim() {
        // A masked unit can still be running; stopping it must be attempted.
        "loaded" | "masked" => UnitLoad::Loaded,
        "not-found" => UnitLoad::NotFound,
        other => UnitLoad::Unknown(format!("unexpected LoadState {other:?}")),
    }
}

async fn unit_load_state(unit: &str) -> UnitLoad {
    let mut cmd = Command::new("systemctl");
    cmd.args(["show", "--property=LoadState", "--value", unit]);
    match cmd.output().await {
        Ok(out) => parse_load_state(
            out.status.success(),
            &String::from_utf8_lossy(&out.stdout),
            &String::from_utf8_lossy(&out.stderr),
        ),
        Err(e) => UnitLoad::Unknown(format!("failed to spawn systemctl: {e}")),
    }
}

/// Firewall presence. `Unknown` blocks opening and is reported on close.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Firewall {
    Absent,
    Present,
    Unknown(String),
}

/// `table_found`: `Some(true)` table listed, `Some(false)` nft said it does
/// not exist, `None` nft is not installed. `other_error` carries any other
/// failure.
fn classify_firewall(table_found: Option<bool>, other_error: Option<String>, config_exists: bool) -> Firewall {
    if let Some(e) = other_error {
        return Firewall::Unknown(e);
    }
    match (table_found, config_exists) {
        (Some(true), _) => Firewall::Present,
        (Some(false), false) | (None, false) => Firewall::Absent,
        (Some(false), true) => Firewall::Unknown(format!(
            "{FIREWALL_CONFIG} exists but table {FIREWALL_FAMILY} {FIREWALL_TABLE} is not loaded (did nftables.service fail?)"
        )),
        (None, true) => Firewall::Unknown(format!("{FIREWALL_CONFIG} exists but nft is not installed")),
    }
}

/// nft reports a missing table, set or element with an ENOENT error line.
fn nft_says_missing(stderr: &str) -> bool {
    stderr
        .lines()
        .any(|line| line.trim_start().starts_with("Error: No such file or directory"))
}

/// The cgroup v2 root marker. A session cgroup directory that does not exist
/// means "no sessions" only on a v2 host; anywhere else the path says nothing.
const CGROUP_V2_MARKER: &str = "/sys/fs/cgroup/cgroup.controllers";

async fn firewall_state() -> Firewall {
    let config_exists = Path::new(FIREWALL_CONFIG).exists();
    let mut cmd = Command::new("nft");
    cmd.args(["list", "table", FIREWALL_FAMILY, FIREWALL_TABLE]);
    match cmd.output().await {
        Ok(out) if out.status.success() => classify_firewall(Some(true), None, config_exists),
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            if nft_says_missing(&stderr) {
                classify_firewall(Some(false), None, config_exists)
            } else {
                classify_firewall(None, Some(format!("nft list table failed: {}", stderr.trim())), config_exists)
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => classify_firewall(None, None, config_exists),
        Err(e) => classify_firewall(None, Some(format!("failed to spawn nft: {e}")), config_exists),
    }
}

fn ssh_port_element() -> String {
    format!("{{ {SSH_PORT} }}")
}

async fn firewall_set_element(verb: &str) -> Result<(), String> {
    let mut cmd = Command::new("nft");
    cmd.args([verb, "element", FIREWALL_FAMILY, FIREWALL_TABLE, MAINTENANCE_PORT_SET])
        .arg(ssh_port_element());
    match cmd.output().await {
        Ok(out) if out.status.success() => Ok(()),
        Ok(out) => Err(String::from_utf8_lossy(&out.stderr).trim().to_string()),
        Err(e) => Err(format!("failed to spawn nft: {e}")),
    }
}

/// `Some(true)` when the maintenance set holds [`SSH_PORT`], `Some(false)`
/// when it does not or the set does not exist, `None` when the answer could
/// not be read.
async fn firewall_port_open() -> Option<bool> {
    let mut cmd = Command::new("nft");
    cmd.args(["-j", "list", "set", FIREWALL_FAMILY, FIREWALL_TABLE, MAINTENANCE_PORT_SET]);
    let out = cmd.output().await.ok()?;
    if !out.status.success() {
        return nft_says_missing(&String::from_utf8_lossy(&out.stderr)).then_some(false);
    }
    set_json_has_port(&String::from_utf8_lossy(&out.stdout), SSH_PORT)
}

/// Read `nft -j list set` output. `None` when it is not the expected shape.
fn set_json_has_port(raw: &str, port: u16) -> Option<bool> {
    let value: serde_json::Value = serde_json::from_str(raw).ok()?;
    let set = value
        .get("nftables")?
        .as_array()?
        .iter()
        .find_map(|entry| entry.get("set"))?;
    let Some(elems) = set.get("elem") else { return Some(false) };
    Some(elems.as_array()?.iter().any(|e| e.as_u64() == Some(u64::from(port))))
}

/// Number of processes anywhere under a cgroup v2 directory; a directory
/// that does not exist holds none.
fn cgroup_process_count(dir: &Path) -> std::io::Result<usize> {
    let mut count = match std::fs::read_to_string(dir.join("cgroup.procs")) {
        Ok(procs) => procs.lines().filter(|l| !l.trim().is_empty()).count(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e),
    };
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            count += cgroup_process_count(&entry.path())?;
        }
    }
    Ok(count)
}

/// Kill every session process under `cgroup` and wait until none is left.
/// `cgroup.kill` (Linux 5.14+) kills the whole subtree atomically.
async fn end_ssh_sessions(cgroup: &Path) -> Result<(), String> {
    if !cgroup.is_dir() {
        if Path::new(CGROUP_V2_MARKER).exists() {
            return Ok(());
        }
        return Err(format!(
            "{} is missing and this host is not on cgroup v2, so open SSH sessions cannot be checked",
            cgroup.display()
        ));
    }
    if let Err(e) = std::fs::write(cgroup.join("cgroup.kill"), "1") {
        return Err(format!("writing {}/cgroup.kill failed: {e}", cgroup.display()));
    }
    for _ in 0..20 {
        match cgroup_process_count(cgroup) {
            Ok(0) => return Ok(()),
            Ok(_) => tokio::time::sleep(std::time::Duration::from_millis(100)).await,
            Err(e) => return Err(format!("reading {} failed: {e}", cgroup.display())),
        }
    }
    match cgroup_process_count(cgroup) {
        Ok(0) => Ok(()),
        Ok(n) => Err(format!("{n} SSH session process(es) still running under {}", cgroup.display())),
        Err(e) => Err(format!("reading {} failed: {e}", cgroup.display())),
    }
}

/// Open the firewall port first, then start the unit, so a firewall that
/// cannot be opened never leaves a listening server the operator cannot
/// reach. A failed unit start takes the port back out.
async fn dispatch_ssh_start() -> DispatchResult {
    let mut chosen = None;
    for candidate in SSH_UNITS {
        match unit_load_state(candidate.unit).await {
            UnitLoad::Loaded => {
                chosen = Some(candidate);
                break;
            }
            UnitLoad::NotFound => continue,
            UnitLoad::Unknown(e) => {
                return Err(SysdError::io(format!("could not check {}: {e}", candidate.unit)));
            }
        }
    }
    let unit = chosen.ok_or_else(|| SysdError::unsupported("this image has no SSH server unit"))?;

    let firewall = match firewall_state().await {
        Firewall::Present => true,
        Firewall::Absent => false,
        Firewall::Unknown(e) => return Err(SysdError::io(format!("firewall state unknown, not opening SSH: {e}"))),
    };
    if firewall {
        if let Err(e) = firewall_set_element("add").await {
            return Err(SysdError::io(format!(
                "could not open tcp/{SSH_PORT} in the firewall; it has no {MAINTENANCE_PORT_SET} set \
                 (DuDuClaw OS images built before 2026-10 and the Debian appliance line lack it): {e}"
            )));
        }
    }

    let mut cmd = Command::new("systemctl");
    cmd.args(["start", unit.unit]);
    let started = run(cmd).await;
    let failed = !matches!(&started, Ok(out) if out.success);
    if failed {
        let rollback = if firewall {
            match firewall_set_element("delete").await {
                Ok(()) => String::new(),
                Err(e) => format!("; closing tcp/{SSH_PORT} again failed: {e}"),
            }
        } else {
            String::new()
        };
        return match started {
            Ok(out) => Ok(SysdOpOutput { stderr: format!("{}{rollback}", out.stderr.trim()), ..out }),
            Err(e) => Err(SysdError::io(format!("{}{rollback}", e.message))),
        };
    }
    let started = started?;
    let firewall_note = if firewall {
        format!("opened tcp/{SSH_PORT} in the firewall")
    } else {
        "no firewall on this image".to_string()
    };
    Ok(SysdOpOutput { stdout: format!("started {}; {firewall_note}", unit.unit), ..started })
}

/// Stop every loaded SSH unit, end its sessions, close the firewall port,
/// then check what is actually left. Each step runs even if an earlier one
/// failed; anything unclear counts as a problem, never as "closed".
async fn dispatch_ssh_stop() -> DispatchResult {
    let mut problems: Vec<String> = Vec::new();
    let mut done: Vec<String> = Vec::new();

    for unit in SSH_UNITS {
        match unit_load_state(unit.unit).await {
            UnitLoad::NotFound => continue,
            UnitLoad::Unknown(e) => {
                problems.push(format!("could not check {}: {e}", unit.unit));
                continue;
            }
            UnitLoad::Loaded => {}
        }
        let mut cmd = Command::new("systemctl");
        cmd.args(["stop", unit.unit]);
        match cmd.output().await {
            Ok(out) if out.status.success() => done.push(format!("stopped {}", unit.unit)),
            Ok(out) => problems.push(format!(
                "systemctl stop {} failed: {}",
                unit.unit,
                String::from_utf8_lossy(&out.stderr).trim()
            )),
            Err(e) => problems.push(format!("failed to spawn systemctl: {e}")),
        }
        if let Some(cgroup) = unit.session_cgroup {
            match end_ssh_sessions(Path::new(cgroup)).await {
                Ok(()) => {}
                Err(e) => problems.push(e),
            }
        }
        if let Some(pattern) = unit.sessions {
            // Clears the now-empty session units; whether sessions really
            // ended was judged from the cgroup above.
            let mut cmd = Command::new("systemctl");
            cmd.args(["stop", pattern]);
            let _ = cmd.output().await;
        }
        let mut cmd = Command::new("systemctl");
        cmd.args(["is-active", unit.unit]);
        match cmd.output().await {
            Ok(out) => match String::from_utf8_lossy(&out.stdout).trim() {
                "inactive" | "failed" => {}
                state => problems.push(format!("{} is still {state:?} after stop", unit.unit)),
            },
            Err(e) => problems.push(format!("failed to spawn systemctl: {e}")),
        }
    }

    match firewall_state().await {
        Firewall::Absent => {}
        Firewall::Unknown(e) => problems.push(format!("firewall state unknown: {e}")),
        Firewall::Present => {
            // Removing a port that is not there fails; the read below decides.
            let _ = firewall_set_element("delete").await;
            match firewall_port_open().await {
                Some(false) => done.push(format!("tcp/{SSH_PORT} not open in the firewall")),
                Some(true) => problems.push(format!("tcp/{SSH_PORT} is still open in the firewall")),
                None => problems.push("could not read the firewall's maintenance port set".to_string()),
            }
        }
    }

    Ok(SysdOpOutput {
        success: problems.is_empty(),
        stdout: if done.is_empty() { "nothing was open".to_string() } else { done.join("; ") },
        stderr: problems.join("; "),
    })
}

/// Dispatch one already-authorized, already-parsed request to its
/// hardcoded command sequence.
pub async fn dispatch(req: &SysdRequest) -> DispatchResult {
    match req {
        SysdRequest::Reboot => {
            let mut cmd = Command::new("systemctl");
            cmd.arg("reboot");
            run(cmd).await
        }
        SysdRequest::Poweroff => {
            let mut cmd = Command::new("systemctl");
            cmd.arg("poweroff");
            run(cmd).await
        }
        SysdRequest::SysupdateStatus => {
            let mut cmd = Command::new(SYSTEMD_SYSUPDATE_BIN);
            cmd.args(["list", "--json=short"]);
            run(cmd).await
        }
        SysdRequest::SysupdateApply => {
            let _guard = begin_update_op(&UPDATE_LOCK)?;
            plain_sysupdate_allowed(crate::rollback_floor::floor())?;
            let mut cmd = Command::new(SYSTEMD_SYSUPDATE_BIN);
            cmd.arg("update");
            run(cmd).await
        }
        SysdRequest::SysupdateStageApply => {
            let _guard = begin_update_op(&UPDATE_LOCK)?;
            dispatch_sysupdate_stage_apply().await
        }
        SysdRequest::BootAssessmentStatus => bless_boot("status").await,
        SysdRequest::UpdateRollback => {
            let _guard = begin_update_op(&UPDATE_LOCK)?;
            dispatch_update_rollback().await
        }
        SysdRequest::FactoryReset => dispatch_factory_reset_v2(false).await,
        SysdRequest::FactoryResetV2 { clear_network } => dispatch_factory_reset_v2(*clear_network).await,
        SysdRequest::ClearNetworkCredentials => dispatch_clear_network_credentials().await,
        SysdRequest::Hostname { set } => dispatch_hostname(set).await,
        SysdRequest::SetTimezone { timezone } => dispatch_set_timezone(timezone).await,
        SysdRequest::SetNtp { enabled } => dispatch_set_ntp(*enabled).await,
        SysdRequest::NetworkWiredConfig {
            interface,
            mode,
            address,
            gateway,
            dns,
        } => {
            dispatch_network_wired_config(
                interface,
                mode,
                address.as_deref(),
                gateway.as_deref(),
                dns,
            )
            .await
        }
        SysdRequest::ClearExhaustedUpdateTarget { version } => {
            // A malformed value is refused before the lock is even tried.
            validate_update_version_syntax(version)?;
            let _guard = begin_update_op(&UPDATE_LOCK)?;
            dispatch_clear_exhausted_update_target(version).await
        }
        SysdRequest::SshServiceStart => dispatch_ssh_start().await,
        SysdRequest::SshServiceStop => dispatch_ssh_stop().await,
        SysdRequest::LocalModelService { action } => dispatch_local_model_service(*action).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn clear_credential_dirs_wipes_every_existing_line_and_refuses_when_none_exist() {
        let root = tempfile::tempdir().unwrap();
        let yocto = root.path().join("system/iwd");
        let debian = root.path().join("network/iwd");
        std::fs::create_dir_all(&yocto).unwrap();
        std::fs::write(yocto.join("home.psk"), "secret").unwrap();

        let cleared = clear_credential_dirs(&[&yocto, &debian]).unwrap();
        assert_eq!(cleared, vec![yocto.display().to_string()]);
        assert!(yocto.is_dir(), "the directory itself survives");
        assert_eq!(std::fs::read_dir(&yocto).unwrap().count(), 0);

        let missing = root.path().join("nowhere");
        let err = clear_credential_dirs(&[&missing, &debian]).unwrap_err();
        assert!(err.contains("no network credential directory found"), "{err}");
    }

    #[test]
    fn maintenance_set_json_is_read_by_element_not_by_substring() {
        let open = r#"{"nftables":[{"metainfo":{"version":"1.1.6"}},{"set":{"family":"inet","name":"maintenance_tcp_ports","table":"filter","type":"inet_service","handle":4,"elem":[22]}}]}"#;
        assert_eq!(set_json_has_port(open, 22), Some(true));
        // An empty set has no "elem" key at all.
        let empty = r#"{"nftables":[{"metainfo":{"version":"1.1.6"}},{"set":{"family":"inet","name":"maintenance_tcp_ports","table":"filter","type":"inet_service","handle":4}}]}"#;
        assert_eq!(set_json_has_port(empty, 22), Some(false));
        // 2222 must not count as 22.
        let other = r#"{"nftables":[{"set":{"name":"maintenance_tcp_ports","elem":[2222,80]}}]}"#;
        assert_eq!(set_json_has_port(other, 22), Some(false));
        assert_eq!(set_json_has_port("not json", 22), None);
        assert_eq!(set_json_has_port(r#"{"nftables":[{"metainfo":{}}]}"#, 22), None);
    }

    #[test]
    fn ssh_units_prefer_dropbear_and_only_it_has_sessions() {
        assert_eq!(SSH_UNITS[0].unit, "dropbear.socket");
        assert_eq!(SSH_UNITS[0].sessions, Some("dropbear@*.service"));
        assert_eq!(
            SSH_UNITS[0].session_cgroup,
            Some("/sys/fs/cgroup/system.slice/system-dropbear.slice")
        );
        assert_eq!(SSH_UNITS[1].unit, "ssh.service");
        assert_eq!(SSH_UNITS[1].sessions, None);
        assert_eq!(ssh_port_element(), "{ 22 }");
    }

    #[test]
    fn only_a_clear_not_found_means_the_unit_is_absent() {
        assert_eq!(parse_load_state(true, "loaded\n", ""), UnitLoad::Loaded);
        assert_eq!(parse_load_state(true, "masked\n", ""), UnitLoad::Loaded);
        assert_eq!(parse_load_state(true, "not-found\n", ""), UnitLoad::NotFound);
        // A D-Bus timeout or empty output is not "no SSH server".
        assert!(matches!(parse_load_state(false, "", "Failed to connect to bus"), UnitLoad::Unknown(_)));
        assert!(matches!(parse_load_state(true, "", ""), UnitLoad::Unknown(_)));
        assert!(matches!(parse_load_state(true, "error\n", ""), UnitLoad::Unknown(_)));
    }

    #[test]
    fn a_configured_firewall_that_did_not_load_is_unknown_not_absent() {
        assert_eq!(classify_firewall(Some(true), None, true), Firewall::Present);
        assert_eq!(classify_firewall(Some(false), None, false), Firewall::Absent);
        assert_eq!(classify_firewall(None, None, false), Firewall::Absent);
        assert!(matches!(classify_firewall(Some(false), None, true), Firewall::Unknown(_)));
        assert!(matches!(classify_firewall(None, None, true), Firewall::Unknown(_)));
        assert!(matches!(
            classify_firewall(None, Some("permission denied".into()), false),
            Firewall::Unknown(_)
        ));
        assert!(nft_says_missing("Error: No such file or directory\nlist table inet filter"));
        assert!(!nft_says_missing("Error: Operation not permitted"));
        assert!(!nft_says_missing("Error: Could not process rule: No such file or directory"));
    }

    #[test]
    fn cgroup_process_count_walks_the_whole_subtree() {
        let root = tempfile::tempdir().unwrap();
        let slice = root.path().join("system-dropbear.slice");
        assert_eq!(cgroup_process_count(&slice).unwrap(), 0, "missing cgroup holds nothing");
        let session = slice.join("dropbear@0-10.0.0.2:22-10.0.0.9:50000.service");
        std::fs::create_dir_all(&session).unwrap();
        std::fs::write(slice.join("cgroup.procs"), "").unwrap();
        std::fs::write(session.join("cgroup.procs"), "4242\n4243\n").unwrap();
        assert_eq!(cgroup_process_count(&slice).unwrap(), 2);
        std::fs::write(session.join("cgroup.procs"), "").unwrap();
        assert_eq!(cgroup_process_count(&slice).unwrap(), 0);
    }

    #[test]
    fn bless_status_is_classified_by_whole_word() {
        assert_eq!(parse_bless_status("indeterminate\n"), BlessState::Indeterminate);
        assert_eq!(parse_bless_status("good\n"), BlessState::Good);
        assert_eq!(parse_bless_status("bad\n"), BlessState::Bad);
        assert_eq!(parse_bless_status("clean\n"), BlessState::Clean);
        assert_eq!(parse_bless_status(""), BlessState::Unknown);
        assert_eq!(
            parse_bless_status("Failed to open EFI variable: No such file"),
            BlessState::Unknown,
            "an error message must never be read as a state"
        );
        // Convention 2: no unanchored substring matching. "goodness" is not
        // the token `good`.
        assert_eq!(parse_bless_status("goodness gracious"), BlessState::Unknown);
    }

    #[test]
    fn boot_counter_suffix_round_trip() {
        assert_eq!(
            split_boot_counter("duduclaw-os_0.2.0+2-1.efi"),
            Some(("duduclaw-os_0.2.0".into(), ".efi".into()))
        );
        assert_eq!(
            split_boot_counter("duduclaw-os_0.2.0+3.efi"),
            Some(("duduclaw-os_0.2.0".into(), ".efi".into()))
        );
        assert_eq!(
            split_boot_counter("duduclaw-os_0.2.0.efi"),
            Some(("duduclaw-os_0.2.0".into(), ".efi".into()))
        );
        // A `+` that is NOT a counter (a Debian kernel version) must survive
        // untouched — mangling it would rename an unrelated entry.
        assert_eq!(
            split_boot_counter("duduclaw-os-6.12.101+deb13-arm64.efi"),
            Some(("duduclaw-os-6.12.101+deb13-arm64".into(), ".efi".into()))
        );
        assert_eq!(split_boot_counter("notaunifiedimage.txt"), None);
    }

    #[test]
    fn exhausted_name_matches_what_bless_boot_would_write() {
        assert_eq!(
            exhausted_name("duduclaw-os_0.2.0.efi").as_deref(),
            Some("duduclaw-os_0.2.0+0-1.efi")
        );
        assert_eq!(
            exhausted_name("duduclaw-os_0.2.0+2-1.efi").as_deref(),
            Some("duduclaw-os_0.2.0+0-1.efi")
        );
        assert!(is_exhausted_entry("duduclaw-os_0.2.0+0-1.efi"));
        assert!(is_exhausted_entry("duduclaw-os_0.2.0+0-3.efi"));
        assert!(!is_exhausted_entry("duduclaw-os_0.2.0+1-2.efi"));
        assert!(!is_exhausted_entry("duduclaw-os_0.2.0.efi"));
    }

    #[test]
    fn rollback_refuses_when_there_is_nothing_to_fall_back_to() {
        let selected = "duduclaw-os_0.2.0.efi".to_string();
        let alone = vec![selected.clone()];
        assert!(check_rollback_target(&alone, &selected).is_err());

        // T7's defence: the other version is already exhausted, so marking
        // this one bad would leave zero healthy entries.
        let both_bad = vec![selected.clone(), "duduclaw-os_0.1.0+0-3.efi".to_string()];
        assert!(check_rollback_target(&both_bad, &selected).is_err());

        let healthy = vec![selected.clone(), "duduclaw-os_0.1.0.efi".to_string()];
        assert!(check_rollback_target(&healthy, &selected).is_ok());

        // The case that actually broke T6 on hardware: on the boot that
        // blesses a new version, LoaderBootCountPath still names the
        // pre-blessing filename while the ESP already holds the renamed one.
        // Identity is the stem, so this must resolve, not refuse.
        let blessed = vec![
            "duduclaw-os_0.2.0.efi".to_string(),
            "duduclaw-os_0.1.0.efi".to_string(),
        ];
        assert!(
            check_rollback_target(&blessed, "duduclaw-os_0.2.0+2-1.efi").is_ok(),
            "a counted name must match the blessed file it became"
        );
        assert_eq!(entry_stem("duduclaw-os_0.2.0+2-1.efi").as_deref(), Some("duduclaw-os_0.2.0"));
        assert_eq!(entry_stem("duduclaw-os_0.2.0.efi").as_deref(), Some("duduclaw-os_0.2.0"));

        let missing = vec!["duduclaw-os_0.1.0.efi".to_string()];
        assert!(check_rollback_target(&missing, &selected).is_err());
    }

    #[test]
    fn boot_count_path_yields_the_entry_filename() {
        assert_eq!(
            boot_count_path_basename("\\EFI\\Linux\\duduclaw-os_0.2.0+2-1.efi").as_deref(),
            Some("duduclaw-os_0.2.0+2-1.efi")
        );
        assert_eq!(
            boot_count_path_basename("/EFI/Linux/duduclaw-os_0.2.0+3.efi").as_deref(),
            Some("duduclaw-os_0.2.0+3.efi")
        );
        // Anything that is not a plain .efi filename is refused rather than
        // joined onto a path.
        for bad in ["", "\\EFI\\Linux\\", "loader/entries/foo.conf", "..\\..\\evil"] {
            assert!(boot_count_path_basename(bad).is_none(), "must refuse {bad:?}");
        }
        // Traversal is neutralised by taking the basename, not by rejecting
        // the string: the result is a plain filename that still has to be
        // present in the ESP before check_rollback_target will act on it.
        assert_eq!(
            boot_count_path_basename("\\EFI\\Linux\\..\\..\\x.efi").as_deref(),
            Some("x.efi")
        );
    }

    /// The three ESP states the QEMU matrix actually produces, pinned so a
    /// probe can never pass for the wrong reason. Every one of these was
    /// observed on the appliance during the 2026-08-24 live-fire round.
    #[test]
    fn observed_esp_states_resolve_the_way_the_matrix_expects() {
        // T6, steady state: the new version was blessed on an earlier boot,
        // so both entries are counter-free. Rollback must be allowed.
        let blessed = vec![
            "duduclaw-os_0.1.0.efi".to_string(),
            "duduclaw-os_0.2.0.efi".to_string(),
        ];
        assert!(check_rollback_target(&blessed, "duduclaw-os_0.2.0.efi").is_ok());
        assert_eq!(
            exhausted_name("duduclaw-os_0.2.0.efi").as_deref(),
            Some("duduclaw-os_0.2.0+0-1.efi")
        );

        // T6, mid-assessment: the file on disk still carries a counter while
        // the boot loader reports the stable id. Both must resolve to one
        // entry — the literal-comparison version of this refused a machine
        // that was running the entry it claimed was absent.
        let counted = vec![
            "duduclaw-os_0.1.0.efi".to_string(),
            "duduclaw-os_0.2.0+2-1.efi".to_string(),
        ];
        assert!(check_rollback_target(&counted, "duduclaw-os_0.2.0.efi").is_ok());

        // T7: every entry exhausted. Must refuse, and specifically for
        // "nothing healthy left" — not for "I cannot find myself".
        let all_bad = vec![
            "duduclaw-os_0.1.0+0-3.efi".to_string(),
            "duduclaw-os_0.2.0+0-3.efi".to_string(),
        ];
        let err = check_rollback_target(&all_bad, "duduclaw-os_0.2.0.efi").unwrap_err();
        assert!(
            err.contains("no other bootable version"),
            "T7 must refuse for the right reason, got: {err}"
        );
    }

    #[test]
    fn entry_filenames_from_firmware_cannot_escape_the_esp() {
        assert!(is_entry_filename("duduclaw-os_0.2.0.efi"));
        for bad in [
            "",
            "../../../etc/passwd.efi",
            "/EFI/Linux/x.efi",
            "sub/dir.efi",
            ".hidden.efi",
            "no-extension",
            "with\0nul.efi",
        ] {
            assert!(!is_entry_filename(bad), "must refuse {bad:?}");
        }
    }

    #[test]
    fn efi_variable_decoding_skips_attributes_and_stops_at_nul() {
        let mut raw = vec![0x07, 0x00, 0x00, 0x00];
        for ch in "duduclaw-os_0.2.0.efi".encode_utf16() {
            raw.extend_from_slice(&ch.to_le_bytes());
        }
        raw.extend_from_slice(&[0, 0]);
        raw.extend_from_slice(b"trailing garbage");
        assert_eq!(
            decode_efi_string(&raw).as_deref(),
            Some("duduclaw-os_0.2.0.efi")
        );
        assert_eq!(decode_efi_string(&[]), None);
        assert_eq!(decode_efi_string(&[1, 2, 3]), None);
        assert_eq!(decode_efi_string(&[7, 0, 0, 0, 0, 0]), None);
    }

    #[tokio::test]
    async fn hostname_rejects_empty_without_spawning() {
        let result = dispatch_hostname("   ").await;
        assert!(matches!(result, Err(e) if e.kind == "bad_request"));
    }

    #[tokio::test]
    async fn hostname_rejects_over_length_without_spawning() {
        let long = "a".repeat(MAX_HOSTNAME_LEN + 1);
        let result = dispatch_hostname(&long).await;
        assert!(matches!(result, Err(e) if e.kind == "bad_request"));
    }

    /// P12: the caller may be an agent. Option-shaped, multi-byte, spaced or
    /// otherwise non-RFC-1123 values never reach `hostnamectl`.
    #[tokio::test]
    async fn hostname_is_a_whitelisted_rfc1123_name() {
        for ok in ["duty-box-01", "a", "box.lan", "  duty-box  ", "x1.y-2.z3"] {
            assert!(validate_hostname_syntax(ok).is_ok(), "{ok}");
        }
        for bad in [
            "-Hroot@evil",
            "--host=root@evil",
            "-M",
            "box -H evil",
            "box;reboot",
            "box\nx",
            "盒子",
            "a..b",
            ".lan",
            "box.",
            "-box",
            "box-",
            "a_b",
            &"a".repeat(64),
        ] {
            let result = dispatch_hostname(bad).await;
            assert!(matches!(result, Err(e) if e.kind == "bad_request"), "{bad:?}");
        }
    }

    #[test]
    fn value_carrying_argv_puts_the_value_after_a_double_dash() {
        // Needles assembled at runtime so this test's own text cannot match.
        let source = include_str!("dispatch.rs");
        let q = '"';
        for (verb, value) in [("set-hostname", "host"), ("set-timezone", "tz"), ("reconfigure", "iface")] {
            let needle = format!("args([{q}{verb}{q}, {q}--{q}, {value}])");
            assert!(source.contains(&needle), "{needle}");
        }
    }

    /// H1: update operations are serialised; a second one while the first
    /// holds the lock is refused immediately (busy), and the first is not
    /// disturbed.
    #[test]
    fn update_operations_are_refused_while_one_runs() {
        static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
        let first = begin_update_op(&LOCK).unwrap();
        match begin_update_op(&LOCK) {
            Err(e) => assert_eq!(e.kind, "busy"),
            Ok(_) => panic!("a second update operation must be refused"),
        }
        drop(first);
        assert!(begin_update_op(&LOCK).is_ok());
    }

    /// The real verbs take the shared lock: while one update operation runs,
    /// a second stage/apply/rollback/clear is refused as busy before doing
    /// anything (no other test in this crate dispatches these verbs).
    #[tokio::test]
    async fn concurrent_update_verbs_are_refused_as_busy() {
        let held = begin_update_op(&UPDATE_LOCK).unwrap();
        for req in [
            SysdRequest::SysupdateStageApply,
            SysdRequest::SysupdateApply,
            SysdRequest::UpdateRollback,
            SysdRequest::ClearExhaustedUpdateTarget { version: "0.5.0".into() },
        ] {
            match dispatch(&req).await {
                Err(e) => assert_eq!(e.kind, "busy", "{req:?}"),
                Ok(out) => panic!("{req:?} ran while another update operation held the lock: {out:?}"),
            }
        }
        drop(held);
    }

    /// L3: plain sysupdate is refused once a rollback floor is set.
    #[test]
    fn plain_sysupdate_is_refused_on_images_with_a_floor() {
        use crate::rollback_floor::{OsVersion, RollbackFloor};
        assert!(plain_sysupdate_allowed(&RollbackFloor::Off).is_ok());
        assert!(plain_sysupdate_allowed(&RollbackFloor::Min(OsVersion::parse("0.4.0").unwrap())).is_err());
        assert!(plain_sysupdate_allowed(&RollbackFloor::Invalid("x".into())).is_err());
    }

    #[test]
    fn factory_reset_path_follows_the_oneshot_unit_file() {
        assert_eq!(reset_path(true), ResetPath::Deferred);
        assert_eq!(reset_path(false), ResetPath::Immediate);
        assert_eq!(marker_content(true), "clear_network=1\n");
        assert_eq!(marker_content(false), "clear_network=0\n");
    }

    #[test]
    fn reset_marker_is_written_atomically_with_mode_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("factory-reset.pending");
        write_reset_marker(&marker, true).unwrap();
        assert_eq!(std::fs::read_to_string(&marker).unwrap(), "clear_network=1\n");
        assert_eq!(std::fs::metadata(&marker).unwrap().permissions().mode() & 0o777, 0o600);
        // Rewritten, not appended; no temp file left behind.
        write_reset_marker(&marker, false).unwrap();
        assert_eq!(std::fs::read_to_string(&marker).unwrap(), "clear_network=0\n");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
        // A stale temp file planted as a symlink is removed, never followed.
        let outside = dir.path().join("outside");
        std::fs::write(&outside, "keep").unwrap();
        std::os::unix::fs::symlink(&outside, dir.path().join(".factory-reset.pending.tmp")).unwrap();
        write_reset_marker(&marker, true).unwrap();
        assert_eq!(std::fs::read_to_string(&outside).unwrap(), "keep");
        assert_eq!(std::fs::read_to_string(&marker).unwrap(), "clear_network=1\n");
    }

    #[test]
    fn local_model_argv_is_two_literals() {
        assert_eq!(local_model_argv(LocalModelAction::Stop), ["stop", "duduclaw-llama-server.service"]);
        assert_eq!(local_model_argv(LocalModelAction::Restart), ["restart", "duduclaw-llama-server.service"]);
    }

    #[test]
    fn gateway_home_wipe_empties_the_directory_and_never_follows_links() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir_all(home.join("agents/a")).unwrap();
        std::fs::write(home.join("config.toml"), "x").unwrap();
        let outside = dir.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("keep"), "x").unwrap();
        std::os::unix::fs::symlink(&outside, home.join("link")).unwrap();
        wipe_gateway_home(&home).unwrap();
        assert!(home.is_dir(), "the directory itself survives");
        assert_eq!(std::fs::read_dir(&home).unwrap().count(), 0);
        assert!(outside.join("keep").exists(), "a symlinked target is not wiped");

        // A home that is itself a symlink is refused, not followed.
        let redirected = dir.path().join("redirected");
        std::os::unix::fs::symlink(&outside, &redirected).unwrap();
        assert!(wipe_gateway_home(&redirected).is_err());
        assert!(outside.join("keep").exists());
        // Missing: nothing to wipe.
        assert!(wipe_gateway_home(&dir.path().join("missing")).is_ok());
        assert_eq!(GATEWAY_HOME_DIR, "/data/duduclaw");
    }

    /// The two systemd helpers this daemon shells out to live in a directory
    /// that is on no service's `PATH`, so they may only ever be spawned by
    /// absolute path. Pinning the literals here makes an accidental
    /// "cleanup" back to a bare name a test failure instead of a runtime
    /// ENOENT that only reproduces on the appliance.
    #[test]
    fn systemd_helper_paths_are_absolute_libexec_paths() {
        for path in [SYSTEMD_SYSUPDATE_BIN, SYSTEMD_BLESS_BOOT_BIN] {
            assert!(
                path.starts_with("/usr/lib/systemd/"),
                "{path} must be an absolute /usr/lib/systemd path — Debian's \
                 systemd-container / systemd-boot packages install these \
                 helpers outside every service PATH"
            );
        }
        assert_eq!(SYSTEMD_SYSUPDATE_BIN, "/usr/lib/systemd/systemd-sysupdate");
        assert_eq!(SYSTEMD_BLESS_BOOT_BIN, "/usr/lib/systemd/systemd-bless-boot");
    }

    /// Regression pin for the *whole module*, not just the two call sites
    /// fixed today: no verb may spawn a `systemd-*` helper by bare name.
    /// The needle is assembled at runtime so this test's own source does not
    /// match itself when the file is scanned.
    #[test]
    fn no_verb_spawns_a_systemd_helper_by_bare_name() {
        let source = include_str!("dispatch.rs");
        let needle = format!("Command::new({}systemd-", '"');
        assert!(
            !source.contains(&needle),
            "a Command::new(\"systemd-…\") call crept back in — spawn systemd \
             helpers by absolute path (see SYSTEMD_SYSUPDATE_BIN)"
        );
    }

    #[tokio::test]
    async fn unsupported_binary_yields_unsupported_kind_not_panic() {
        // A command that (almost certainly) doesn't exist on the test host —
        // must degrade to a structured error, never panic the connection task.
        let cmd = Command::new("duduclaw-sysd-test-nonexistent-binary-xyz");
        let result = run(cmd).await;
        assert!(matches!(result, Err(e) if e.kind == "unsupported"));
    }

    // --- set_timezone -------------------------------------------------

    #[test]
    fn timezone_syntax_accepts_ordinary_values() {
        assert_eq!(
            validate_timezone_syntax("Asia/Taipei").unwrap(),
            "Asia/Taipei"
        );
        assert_eq!(validate_timezone_syntax("UTC").unwrap(), "UTC");
        assert_eq!(
            validate_timezone_syntax("America/Argentina/Buenos_Aires").unwrap(),
            "America/Argentina/Buenos_Aires"
        );
        assert_eq!(
            validate_timezone_syntax("  Asia/Taipei  ").unwrap(),
            "Asia/Taipei"
        );
    }

    #[test]
    fn timezone_syntax_rejects_empty() {
        assert!(matches!(validate_timezone_syntax(""), Err(e) if e.kind == "bad_request"));
        assert!(matches!(validate_timezone_syntax("   "), Err(e) if e.kind == "bad_request"));
    }

    #[test]
    fn timezone_syntax_rejects_over_length() {
        let long = "A".repeat(MAX_TIMEZONE_LEN + 1);
        assert!(matches!(validate_timezone_syntax(&long), Err(e) if e.kind == "bad_request"));
    }

    #[test]
    fn timezone_syntax_rejects_non_ascii() {
        assert!(
            matches!(validate_timezone_syntax("Asia/Taipei\u{00e9}"), Err(e) if e.kind == "bad_request")
        );
    }

    #[test]
    fn timezone_syntax_rejects_leading_slash() {
        assert!(
            matches!(validate_timezone_syntax("/Asia/Taipei"), Err(e) if e.kind == "bad_request")
        );
    }

    #[test]
    fn timezone_syntax_rejects_trailing_slash() {
        assert!(
            matches!(validate_timezone_syntax("Asia/Taipei/"), Err(e) if e.kind == "bad_request")
        );
    }

    #[test]
    fn timezone_syntax_rejects_traversal() {
        assert!(
            matches!(validate_timezone_syntax("../../etc/passwd"), Err(e) if e.kind == "bad_request")
        );
    }

    #[test]
    fn timezone_syntax_rejects_double_slash() {
        assert!(
            matches!(validate_timezone_syntax("Asia//Taipei"), Err(e) if e.kind == "bad_request")
        );
    }

    #[test]
    fn timezone_syntax_rejects_too_many_segments() {
        assert!(matches!(validate_timezone_syntax("a/b/c/d"), Err(e) if e.kind == "bad_request"));
    }

    #[test]
    fn timezone_syntax_rejects_disallowed_characters() {
        assert!(
            matches!(validate_timezone_syntax("Asia/Taipei; rm -rf /"), Err(e) if e.kind == "bad_request")
        );
        assert!(
            matches!(validate_timezone_syntax("Asia/Taipei$(whoami)"), Err(e) if e.kind == "bad_request")
        );
    }

    /// `timezone_exists` against a fully controlled temp dir — never the
    /// real `/usr/share/zoneinfo`, so this passes regardless of whether
    /// this test host happens to ship a real zoneinfo database.
    #[test]
    fn timezone_exists_checks_containment_and_file_type() {
        let dir = tempfile::tempdir().unwrap();
        let asia = dir.path().join("Asia");
        std::fs::create_dir(&asia).unwrap();
        std::fs::write(asia.join("Taipei"), b"fake tzdata").unwrap();

        assert!(timezone_exists(dir.path(), "Asia/Taipei"));
        // Not present in this fake db.
        assert!(!timezone_exists(dir.path(), "Asia/Tokyo"));
        // A directory, not a regular file, must not count as "exists".
        assert!(!timezone_exists(dir.path(), "Asia"));
    }

    #[test]
    fn timezone_exists_is_false_when_root_is_missing() {
        let missing = std::path::Path::new("/duduclaw-sysd-test-no-such-zoneinfo-root");
        assert!(!timezone_exists(missing, "Asia/Taipei"));
    }

    #[tokio::test]
    async fn set_timezone_rejects_traversal_without_touching_the_filesystem() {
        // Fails the pure syntax check first, so this is deterministic on
        // every host regardless of what `/usr/share/zoneinfo` looks like.
        let result = dispatch_set_timezone("../../etc/passwd").await;
        assert!(matches!(result, Err(e) if e.kind == "bad_request"));
    }

    #[tokio::test]
    async fn set_timezone_rejects_empty_without_touching_the_filesystem() {
        let result = dispatch_set_timezone("").await;
        assert!(matches!(result, Err(e) if e.kind == "bad_request"));
    }

    #[tokio::test]
    async fn set_timezone_with_valid_syntax_never_panics_regardless_of_host_zoneinfo() {
        // Deliberately tolerant of the real host's `/usr/share/zoneinfo`
        // state (present or absent, `timedatectl` present or absent) —
        // the contract under test is "never ok:true with the wrong verb
        // outcome and never a panic", not a specific error kind.
        let result = dispatch_set_timezone("Asia/Taipei").await;
        if let Err(e) = result {
            assert!(
                e.kind == "unsupported" || e.kind == "bad_request",
                "unexpected error kind: {e:?}"
            );
        }
    }

    // --- set_ntp --------------------------------------------------------

    #[test]
    fn ntp_arg_maps_bool_to_the_literal_strings() {
        assert_eq!(ntp_arg(true), "true");
        assert_eq!(ntp_arg(false), "false");
    }

    // --- network_wired_config: interface -------------------------------

    #[test]
    fn interface_syntax_accepts_ordinary_names() {
        assert_eq!(validate_interface_syntax("enp1s0").unwrap(), "enp1s0");
        assert_eq!(validate_interface_syntax("eth0").unwrap(), "eth0");
    }

    #[test]
    fn interface_syntax_rejects_empty_and_over_length() {
        assert!(matches!(validate_interface_syntax(""), Err(e) if e.kind == "bad_request"));
        let long = "a".repeat(MAX_INTERFACE_LEN + 1);
        assert!(matches!(validate_interface_syntax(&long), Err(e) if e.kind == "bad_request"));
    }

    #[test]
    fn interface_syntax_rejects_disallowed_characters() {
        assert!(
            matches!(validate_interface_syntax("enp1s0; rm -rf /"), Err(e) if e.kind == "bad_request")
        );
        assert!(matches!(validate_interface_syntax("../etc"), Err(e) if e.kind == "bad_request"));
        assert!(matches!(validate_interface_syntax("eth0/../"), Err(e) if e.kind == "bad_request"));
    }

    #[test]
    fn interface_exists_checks_containment() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("enp1s0"), b"fake sysfs entry").unwrap();
        assert!(interface_exists(dir.path(), "enp1s0"));
        assert!(!interface_exists(dir.path(), "eth99"));
        assert!(!interface_exists(dir.path(), "../enp1s0"));
        // Real sysfs: the entry is a symlink into /sys/devices/….
        let devices = tempfile::tempdir().unwrap();
        std::fs::create_dir(devices.path().join("eth0")).unwrap();
        std::os::unix::fs::symlink(devices.path().join("eth0"), dir.path().join("eth0")).unwrap();
        assert!(interface_exists(dir.path(), "eth0"));
    }

    #[test]
    fn interface_and_timezone_values_never_read_as_options() {
        for bad in ["-H", "--help", "_x"] {
            assert!(matches!(validate_interface_syntax(bad), Err(e) if e.kind == "bad_request"), "{bad}");
        }
        for bad in ["-Hroot", "Asia/-x", "+0800", "Etc/.hidden", "_x/y"] {
            assert!(matches!(validate_timezone_syntax(bad), Err(e) if e.kind == "bad_request"), "{bad}");
        }
        for ok in ["Etc/GMT+8", "Etc/GMT-8", "America/Port-au-Prince", "Asia/Taipei"] {
            assert!(validate_timezone_syntax(ok).is_ok(), "{ok}");
        }
    }

    #[test]
    fn interface_exists_is_false_when_root_is_missing() {
        let missing = std::path::Path::new("/duduclaw-sysd-test-no-such-sys-class-net");
        assert!(!interface_exists(missing, "enp1s0"));
    }

    #[tokio::test]
    async fn network_wired_config_rejects_bad_interface_without_touching_the_filesystem() {
        let result = dispatch_network_wired_config("bad iface!", "dhcp", None, None, &[]).await;
        assert!(matches!(result, Err(e) if e.kind == "bad_request"));
    }

    // --- network_wired_config: mode -------------------------------------

    #[test]
    fn wired_mode_parses_the_two_known_values() {
        assert_eq!(WiredMode::parse("dhcp").unwrap(), WiredMode::Dhcp);
        assert_eq!(WiredMode::parse("static").unwrap(), WiredMode::Static);
    }

    #[test]
    fn wired_mode_rejects_anything_else() {
        assert!(matches!(WiredMode::parse("bogus"), Err(e) if e.kind == "bad_request"));
        assert!(matches!(WiredMode::parse(""), Err(e) if e.kind == "bad_request"));
        assert!(matches!(WiredMode::parse("DHCP"), Err(e) if e.kind == "bad_request"));
    }

    #[tokio::test]
    async fn network_wired_config_rejects_unknown_mode_without_touching_the_filesystem() {
        // Interface syntax is valid, so this exercises the mode check —
        // and it must fail BEFORE any `/sys/class/net` access, since this
        // dev Mac has no such tree at all.
        let result = dispatch_network_wired_config("enp1s0", "bogus", None, None, &[]).await;
        assert!(matches!(result, Err(e) if e.kind == "bad_request"));
    }

    // --- network_wired_config: static address/gateway/dns --------------

    #[test]
    fn parse_ipv4_with_prefix_accepts_valid_values() {
        let (ip, prefix) = parse_ipv4_with_prefix("192.168.1.50/24").unwrap();
        assert_eq!(ip, Ipv4Addr::new(192, 168, 1, 50));
        assert_eq!(prefix, 24);
    }

    #[test]
    fn parse_ipv4_with_prefix_rejects_missing_prefix() {
        assert!(
            matches!(parse_ipv4_with_prefix("192.168.1.50"), Err(e) if e.kind == "bad_request")
        );
    }

    #[test]
    fn parse_ipv4_with_prefix_rejects_out_of_range_prefix() {
        assert!(
            matches!(parse_ipv4_with_prefix("192.168.1.50/0"), Err(e) if e.kind == "bad_request")
        );
        assert!(
            matches!(parse_ipv4_with_prefix("192.168.1.50/33"), Err(e) if e.kind == "bad_request")
        );
    }

    #[test]
    fn parse_ipv4_with_prefix_rejects_ipv6() {
        let result = parse_ipv4_with_prefix("2001:db8::1/64");
        match result {
            Err(e) => {
                assert_eq!(e.kind, "bad_request");
                assert!(
                    e.message.contains("IPv6"),
                    "message should mention IPv6: {}",
                    e.message
                );
            }
            Ok(_) => panic!("IPv6 address must be rejected"),
        }
    }

    #[test]
    fn parse_ipv4_with_prefix_rejects_garbage_address() {
        assert!(
            matches!(parse_ipv4_with_prefix("not-an-ip/24"), Err(e) if e.kind == "bad_request")
        );
    }

    #[test]
    fn parse_ipv4_field_rejects_ipv6_gateway() {
        let result = parse_ipv4_field("2001:db8::1", "gateway");
        match result {
            Err(e) => {
                assert_eq!(e.kind, "bad_request");
                assert!(e.message.contains("IPv6"));
            }
            Ok(_) => panic!("IPv6 gateway must be rejected"),
        }
    }

    #[test]
    fn parse_ipv4_field_accepts_valid_gateway() {
        assert_eq!(
            parse_ipv4_field("192.168.1.1", "gateway").unwrap(),
            Ipv4Addr::new(192, 168, 1, 1)
        );
    }

    #[test]
    fn parse_dns_entries_accepts_up_to_three_mixed_family_entries() {
        let entries = vec![
            "192.168.1.1".to_string(),
            "1.1.1.1".to_string(),
            "2001:db8::1".to_string(),
        ];
        let parsed = parse_dns_entries(&entries).unwrap();
        assert_eq!(parsed.len(), 3);
    }

    #[test]
    fn parse_dns_entries_rejects_more_than_three() {
        let entries = vec![
            "192.168.1.1".to_string(),
            "1.1.1.1".to_string(),
            "8.8.8.8".to_string(),
            "9.9.9.9".to_string(),
        ];
        assert!(matches!(parse_dns_entries(&entries), Err(e) if e.kind == "bad_request"));
    }

    #[test]
    fn parse_dns_entries_rejects_garbage_entry() {
        let entries = vec!["not-an-ip".to_string()];
        assert!(matches!(parse_dns_entries(&entries), Err(e) if e.kind == "bad_request"));
    }

    #[tokio::test]
    async fn network_wired_config_static_requires_address() {
        let result = dispatch_network_wired_config("enp1s0", "static", None, None, &[]).await;
        assert!(matches!(result, Err(e) if e.kind == "bad_request"));
    }

    #[tokio::test]
    async fn network_wired_config_static_rejects_bad_address_without_touching_the_filesystem() {
        let result =
            dispatch_network_wired_config("enp1s0", "static", Some("not-an-ip/24"), None, &[])
                .await;
        assert!(matches!(result, Err(e) if e.kind == "bad_request"));
    }

    #[tokio::test]
    async fn network_wired_config_static_rejects_ipv6_address_without_touching_the_filesystem() {
        let result =
            dispatch_network_wired_config("enp1s0", "static", Some("2001:db8::1/64"), None, &[])
                .await;
        match result {
            Err(e) => {
                assert_eq!(e.kind, "bad_request");
                assert!(e.message.contains("IPv6"));
            }
            Ok(_) => panic!("IPv6 address must be rejected"),
        }
    }

    #[tokio::test]
    async fn network_wired_config_static_rejects_bad_gateway_without_touching_the_filesystem() {
        let result = dispatch_network_wired_config(
            "enp1s0",
            "static",
            Some("192.168.1.50/24"),
            Some("not-a-gateway"),
            &[],
        )
        .await;
        assert!(matches!(result, Err(e) if e.kind == "bad_request"));
    }

    #[tokio::test]
    async fn network_wired_config_static_rejects_too_many_dns_entries() {
        let dns = vec![
            "192.168.1.1".to_string(),
            "1.1.1.1".to_string(),
            "8.8.8.8".to_string(),
            "9.9.9.9".to_string(),
        ];
        let result =
            dispatch_network_wired_config("enp1s0", "static", Some("192.168.1.50/24"), None, &dns)
                .await;
        assert!(matches!(result, Err(e) if e.kind == "bad_request"));
    }

    // --- render_wired_network --------------------------------------------

    #[test]
    fn render_wired_network_dhcp_shape_has_no_gateway_or_dns() {
        // "dhcp mode" here refers to the render call never being made for
        // that mode in `dispatch_network_wired_config` (it removes the
        // file instead) — this test covers the "no gateway, no dns"
        // shape of the render function itself, i.e. a static config
        // without either optional field.
        let out = render_wired_network("enp1s0", Ipv4Addr::new(192, 168, 1, 50), 24, None, &[]);
        assert_eq!(
            out,
            "[Match]\nName=enp1s0\n\n[Network]\nDHCP=no\nAddress=192.168.1.50/24\nIPv6AcceptRA=no\n"
        );
        assert!(!out.contains("Gateway"));
        assert!(!out.contains("[Route]"));
    }

    #[test]
    fn render_wired_network_static_shape_with_gateway_and_dns() {
        let dns = [
            IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1)),
            IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1)),
        ];
        let out = render_wired_network(
            "enp1s0",
            Ipv4Addr::new(192, 168, 1, 50),
            24,
            Some(Ipv4Addr::new(192, 168, 1, 1)),
            &dns,
        );
        assert_eq!(
            out,
            "[Match]\n\
             Name=enp1s0\n\
             \n\
             [Network]\n\
             DHCP=no\n\
             Address=192.168.1.50/24\n\
             Gateway=192.168.1.1\n\
             DNS=192.168.1.1\n\
             DNS=1.1.1.1\n\
             IPv6AcceptRA=no\n\
             \n\
             [Route]\n\
             Gateway=192.168.1.1\n\
             Metric=100\n"
        );
    }

    #[test]
    fn render_wired_network_output_is_strictly_ascii() {
        let dns = [IpAddr::V6("2001:db8::1".parse().unwrap())];
        let out = render_wired_network(
            "enp1s0",
            Ipv4Addr::new(10, 0, 0, 5),
            8,
            Some(Ipv4Addr::new(10, 0, 0, 1)),
            &dns,
        );
        assert!(
            out.is_ascii(),
            "rendered .network content must be strictly ASCII: {out:?}"
        );
    }

    // --- wired static config file write/remove --------------------------

    #[tokio::test]
    async fn remove_wired_static_config_on_a_missing_file_is_success() {
        let dir = tempfile::tempdir().unwrap();
        // No file was ever written in this dir — removal must still be Ok.
        let result = remove_wired_static_config(dir.path()).await;
        assert!(
            result.is_ok(),
            "removing an absent override file must be success: {result:?}"
        );
    }

    #[tokio::test]
    async fn remove_wired_static_config_removes_an_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(WIRED_NETWORK_FILENAME);
        std::fs::write(&path, b"stale content").unwrap();
        assert!(path.exists());

        let result = remove_wired_static_config(dir.path()).await;
        assert!(result.is_ok());
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn write_wired_static_config_writes_atomically_and_leaves_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let content = render_wired_network("enp1s0", Ipv4Addr::new(192, 168, 1, 50), 24, None, &[]);

        write_wired_static_config(dir.path(), &content)
            .await
            .unwrap();

        let final_path = dir.path().join(WIRED_NETWORK_FILENAME);
        let tmp_path = dir.path().join(format!("{WIRED_NETWORK_FILENAME}.tmp"));
        assert!(final_path.exists());
        assert!(
            !tmp_path.exists(),
            "temp file must be renamed away, not left behind"
        );
        assert_eq!(std::fs::read_to_string(&final_path).unwrap(), content);

        let mode = std::fs::metadata(&final_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o644, "override file must be 0644");
    }

    #[tokio::test]
    async fn write_wired_static_config_overwrites_a_previous_config() {
        let dir = tempfile::tempdir().unwrap();
        write_wired_static_config(dir.path(), "old content\n")
            .await
            .unwrap();
        write_wired_static_config(dir.path(), "new content\n")
            .await
            .unwrap();

        let final_path = dir.path().join(WIRED_NETWORK_FILENAME);
        assert_eq!(
            std::fs::read_to_string(&final_path).unwrap(),
            "new content\n"
        );
    }

    // --- M1: wipe_dir_contents (kiosk-home + network-credentials wipes) --

    #[test]
    fn wipe_dir_contents_removes_files_and_subdirs_but_not_dir_itself() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("oobe_state.json"), b"{}").unwrap();
        std::fs::create_dir(dir.path().join("shell")).unwrap();
        std::fs::write(dir.path().join("shell/nested.json"), b"{}").unwrap();

        wipe_dir_contents(dir.path()).unwrap();

        assert!(dir.path().exists(), "the directory itself must survive");
        let remaining: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
        assert!(remaining.is_empty(), "contents must be gone: {remaining:?}");
    }

    #[test]
    fn wipe_dir_contents_missing_dir_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("does-not-exist");
        assert!(wipe_dir_contents(&missing).is_ok());
    }

    #[test]
    fn wipe_dir_contents_on_an_already_empty_dir_is_a_noop() {
        let dir = tempfile::tempdir().unwrap();
        assert!(wipe_dir_contents(dir.path()).is_ok());
        assert!(dir.path().exists());
    }

    // --- M1: dispatch_clear_network_credentials (via wipe_dir_contents) --

    #[test]
    fn network_credentials_dir_wipe_leaves_the_directory_itself_in_place() {
        // Exercises the exact helper `dispatch_clear_network_credentials`
        // calls, against a stand-in for `/data/network/iwd` — the real
        // path is root-owned and not writable in a test process, so this
        // proves the underlying wipe semantics (dir survives, credential
        // files inside do not) the same way `remove_wired_static_config`'s
        // tests above prove their own hardcoded-path sibling's behavior.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("MyHomeWiFi.psk"), b"secret").unwrap();
        std::fs::write(dir.path().join("OfficeAP.psk"), b"secret2").unwrap();

        wipe_dir_contents(dir.path()).unwrap();

        assert!(dir.path().exists());
        assert!(std::fs::read_dir(dir.path()).unwrap().next().is_none());
    }

    // --- H3d §11.7: clear_exhausted_update_target ------------------------

    #[test]
    fn update_version_syntax_accepts_ordinary_semver_and_rejects_junk() {
        assert_eq!(validate_update_version_syntax("0.2.0").unwrap(), "0.2.0");
        assert_eq!(
            validate_update_version_syntax("1.0.0-rc1+build.5").unwrap(),
            "1.0.0-rc1+build.5"
        );
        assert_eq!(validate_update_version_syntax("  0.2.0  ").unwrap(), "0.2.0");

        let long = "a".repeat(MAX_UPDATE_VERSION_LEN + 1);
        for bad in [
            "",
            "   ",
            "../../etc/passwd",
            "0.2.0; rm -rf /",
            "0.2.0 rm",
            ".hidden",
            "-leading",
            "+leading",
            long.as_str(),
        ] {
            assert!(
                validate_update_version_syntax(bad).is_err(),
                "must reject {bad:?}"
            );
        }
    }

    #[test]
    fn find_exhausted_entry_only_matches_the_exhausted_shape_of_the_target_stem() {
        let entries = vec![
            "duduclaw-os_0.1.0.efi".to_string(),
            "duduclaw-os_0.2.0+0-1.efi".to_string(),
        ];
        assert_eq!(
            find_exhausted_entry(&entries, "duduclaw-os_0.2.0").map(String::as_str),
            Some("duduclaw-os_0.2.0+0-1.efi"),
            "the rolled-back version's exhausted entry must be found"
        );
        // A healthy/blessed entry for a DIFFERENT stem must never match —
        // nothing stale to clear for it.
        assert_eq!(find_exhausted_entry(&entries, "duduclaw-os_0.1.0"), None);
        // No entry at all for a third version — also nothing to do.
        assert_eq!(find_exhausted_entry(&entries, "duduclaw-os_0.3.0"), None);
    }

    #[test]
    fn find_exhausted_entry_leaves_live_boot_assessment_state_alone() {
        // +2-1 (tries still left) and no suffix (blessed) are both LIVE
        // state, not stale leftovers — must never be reported as
        // "exhausted", or this verb would delete a boot entry that is
        // actively being evaluated or already healthy.
        let mid_assessment = vec!["duduclaw-os_0.2.0+2-1.efi".to_string()];
        assert_eq!(find_exhausted_entry(&mid_assessment, "duduclaw-os_0.2.0"), None);

        let blessed = vec!["duduclaw-os_0.2.0.efi".to_string()];
        assert_eq!(find_exhausted_entry(&blessed, "duduclaw-os_0.2.0"), None);
    }

    #[test]
    fn target_is_running_entry_refuses_the_machines_own_boot_entry() {
        assert!(target_is_running_entry(
            Some("duduclaw-os_0.2.0.efi"),
            "duduclaw-os_0.2.0"
        ));
        // A counted filename must still resolve through the stem — this is
        // the exact drift `check_rollback_target` also has to handle.
        assert!(target_is_running_entry(
            Some("duduclaw-os_0.2.0+2-1.efi"),
            "duduclaw-os_0.2.0"
        ));
        assert!(!target_is_running_entry(
            Some("duduclaw-os_0.1.0.efi"),
            "duduclaw-os_0.2.0"
        ));
        assert!(!target_is_running_entry(None, "duduclaw-os_0.2.0"));
    }

    #[tokio::test]
    async fn clear_exhausted_update_target_rejects_bad_version_without_touching_the_esp() {
        let result = dispatch_clear_exhausted_update_target("../../etc/passwd").await;
        assert!(matches!(result, Err(e) if e.kind == "bad_request"));
    }
}
