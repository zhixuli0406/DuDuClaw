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
/// [`SYSTEMD_SYSUPDATE_BIN`]. Nothing dispatches it yet — the
/// `UpdateRollback` / `BootAssessmentStatus` verbs are H3f's job — but the
/// constant is pinned here (and covered by a test) so the next verb cannot
/// repeat the bare-name mistake this module just fixed.
#[cfg_attr(not(test), allow(dead_code))]
const SYSTEMD_BLESS_BOOT_BIN: &str = "/usr/lib/systemd/systemd-bless-boot";

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

/// `systemctl enable duduclaw-firstboot-provision.service` then
/// `systemctl reboot`. The enable step is best-effort — an image without
/// that unit (or a dev/test host) should still complete the reboot rather
/// than abort the whole factory-reset flow; a failure there is folded into
/// the final `stdout` as a `[warn]` line, mirroring the equivalent note
/// `SystemDeviceOps::factory_reset` used to build itself before this
/// verb existed.
async fn dispatch_factory_reset() -> DispatchResult {
    let mut enable_cmd = Command::new("systemctl");
    enable_cmd.args(["enable", "duduclaw-firstboot-provision.service"]);
    let enable = enable_cmd.output().await;
    let warn_note = match &enable {
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
    Ok(SysdOpOutput { stdout: format!("{}{warn_note}", reboot.stdout), ..reboot })
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
        SysdRequest::FactoryReset => dispatch_factory_reset().await,
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

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
}
