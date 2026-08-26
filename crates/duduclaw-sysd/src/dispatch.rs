//! Verb → hardcoded shell-out dispatch.
//!
//! Every `Command::new(...)` argument here is a literal, exactly the
//! discipline `duduclaw-gateway/src/device_ops.rs` documents for its own
//! `SystemDeviceOps` — this module is that same rule applied on the root
//! side of the privilege boundary. The only caller-supplied value that
//! ever reaches a spawned process is [`SysdRequest::Hostname`]'s `set`
//! field, and it is passed via `Command::arg()` (never a shell), so its
//! content can only ever be *the hostname value*, never *which command
//! runs*.

use std::net::{IpAddr, Ipv4Addr};
use std::path::Path;

use tokio::process::Command;

use crate::protocol::{MAX_HOSTNAME_LEN, MAX_TIMEZONE_LEN, SysdError, SysdOpOutput, SysdRequest};

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

/// Directory whose CONTENTS are iwd's persisted Wi-Fi credential files
/// (`/data/network/iwd`, 0700 root:root — see
/// `appliance/mkosi.extra/usr/lib/tmpfiles.d/duduclaw-network.conf`). The
/// directory itself is left in place (tmpfiles recreates it unconditionally
/// every boot); only the credential files inside are removed, mirroring
/// `duduclaw-gateway/src/device_ops.rs`'s own `wipe_dir_contents` for
/// `home_dir` — same "the mount point survives, what's inside does not"
/// discipline this module's [`wipe_dir_contents`] below implements.
const NETWORK_CREDENTIALS_DIR: &str = "/data/network/iwd";

/// Remove every entry directly under `dir` (not `dir` itself). Missing
/// `dir` is not an error (nothing to wipe) — mirrors
/// `duduclaw-gateway/src/device_ops.rs`'s identically-named helper for
/// `home_dir`; this is that same discipline applied on the root side of
/// the privilege boundary, for the two paths ([`KIOSK_HOME_DIR`],
/// [`NETWORK_CREDENTIALS_DIR`]) the unprivileged gateway process cannot
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

/// Wipe the CONTENTS of [`NETWORK_CREDENTIALS_DIR`] — see that constant's
/// doc comment for what lives there and why the directory entry itself
/// survives. Unlike the kiosk-home wipe in [`dispatch_factory_reset`], a
/// failure here is returned as a hard `Err` rather than folded into a
/// `[warn]` note: this verb only ever runs when an operator explicitly
/// opted in to "一併清除網路設定", and the gateway-side caller
/// (`SysdDeviceOps::factory_reset`) treats that opt-in as fail-closed —
/// silently downgrading a real failure to a warning would let the operator
/// walk away believing saved Wi-Fi credentials are gone when they are not.
async fn dispatch_clear_network_credentials() -> DispatchResult {
    match wipe_dir_contents(Path::new(NETWORK_CREDENTIALS_DIR)) {
        Ok(()) => Ok(SysdOpOutput {
            success: true,
            stdout: format!("cleared {NETWORK_CREDENTIALS_DIR}"),
            stderr: String::new(),
        }),
        Err(e) => Err(SysdError::io(format!(
            "clearing {NETWORK_CREDENTIALS_DIR} failed: {e}"
        ))),
    }
}

/// `hostnamectl set-hostname <name>`. Rejects an empty or over-length
/// value as a structured `bad_request` before ever spawning anything —
/// `Command::arg()` is already injection-safe regardless of content, this
/// check exists purely to refuse an obviously-wrong request early.
async fn dispatch_hostname(name: &str) -> DispatchResult {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(SysdError::bad_request("hostname value must not be empty"));
    }
    if trimmed.chars().count() > MAX_HOSTNAME_LEN {
        return Err(SysdError::bad_request(format!(
            "hostname value exceeds {MAX_HOSTNAME_LEN} chars"
        )));
    }
    let mut cmd = Command::new("hostnamectl");
    cmd.args(["set-hostname", trimmed]);
    run(cmd).await
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
    cmd.args(["set-timezone", tz]);
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
    Ok(trimmed)
}

/// Whitelist check: `<root>/<iface>` must canonicalize to a path still
/// contained under the canonicalized root — the same containment
/// discipline [`timezone_exists`] uses, applied to `/sys/class/net`'s
/// per-interface symlinks. `root` is a parameter so tests can point this
/// at a temp dir instead of the real sysfs tree.
fn interface_exists(root: &Path, iface: &str) -> bool {
    let root_canon = match root.canonicalize() {
        Ok(p) => p,
        Err(_) => return false,
    };
    match root.join(iface).canonicalize() {
        Ok(p) => p.starts_with(&root_canon),
        Err(_) => false,
    }
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
    reconfigure_cmd.args(["reconfigure", iface]);
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
    let (_dir, entries) = read_esp_entries().await?;
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
            let mut cmd = Command::new(SYSTEMD_SYSUPDATE_BIN);
            cmd.arg("update");
            run(cmd).await
        }
        SysdRequest::BootAssessmentStatus => bless_boot("status").await,
        SysdRequest::UpdateRollback => dispatch_update_rollback().await,
        SysdRequest::FactoryReset => dispatch_factory_reset().await,
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
            dispatch_clear_exhausted_update_target(version).await
        }
        SysdRequest::SshServiceStart => {
            let mut cmd = Command::new("systemctl");
            cmd.args(["start", "ssh.service"]);
            run(cmd).await
        }
        SysdRequest::SshServiceStop => {
            let mut cmd = Command::new("systemctl");
            cmd.args(["stop", "ssh.service"]);
            run(cmd).await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

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
