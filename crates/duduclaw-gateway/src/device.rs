//! Appliance device status/management — the data-gathering + orchestration
//! half of the `device.*` dashboard RPCs (dispatch + admin/appliance gating
//! lives in `handlers.rs`; shell-outs live in `device_ops.rs`).
//!
//! Every "read a live number" function here follows the same split as
//! `duduclaw-inference/src/hardware.rs`: a thin OS-facing collector (reads a
//! file / makes a syscall, not unit-tested beyond "doesn't panic") feeding a
//! pure parsing/computation function (unit-tested with synthetic input on
//! any host, including this repo's macOS dev machines). The appliance image
//! itself is Debian/Linux-only, but the crate still has to *compile* for
//! the desktop macOS/Windows targets, so every collector degrades to `None`
//! rather than failing to build off-Linux.

use std::path::Path;

use serde::Serialize;

/// Full device status snapshot for `device.status`.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct DeviceStatus {
    pub cpu_cores: usize,
    /// 1/5/15-minute load averages from `/proc/loadavg`. `None` off-Linux.
    pub load_average: Option<LoadAverage>,
    pub ram: Option<MemInfo>,
    pub disk: Option<DiskUsage>,
    /// First readable `/sys/class/thermal/thermal_zone*/temp`, in Celsius.
    /// `None` when no thermal zone is readable (off-Linux, or a Linux host
    /// with no exposed thermal sensor).
    pub temperature_c: Option<f64>,
    pub uptime_secs: Option<u64>,
    pub network_interfaces: Vec<NetworkInterface>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq)]
pub struct LoadAverage {
    pub load1: f64,
    pub load5: f64,
    pub load15: f64,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
pub struct MemInfo {
    pub total_mb: u64,
    /// `MemAvailable` from `/proc/meminfo` — the kernel's own reclaimable-
    /// cache-aware estimate, not a naive "free" count (matches
    /// `duduclaw-inference/src/hardware.rs`'s Linux RAM detection).
    pub available_mb: u64,
    pub used_mb: u64,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
pub struct DiskUsage {
    pub total_mb: u64,
    pub used_mb: u64,
    pub available_mb: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct NetworkInterface {
    pub name: String,
    pub is_up: bool,
    pub addresses: Vec<String>,
}

// ── RAM (/proc/meminfo) ─────────────────────────────────────────────────

/// Parse one `MemTotal:`/`MemAvailable:`-style line's kB value out of
/// `/proc/meminfo` text. Pure — mirrors
/// `duduclaw-inference::hardware::parse_meminfo_kb` (not reused directly
/// across crates for a 5-line helper; same technique).
fn parse_meminfo_kb(text: &str, field: &str) -> Option<u64> {
    text.lines()
        .find(|l| l.starts_with(field))
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|v| v.parse::<u64>().ok())
}

/// Pure: build [`MemInfo`] from already-read `/proc/meminfo` text. `None`
/// when either field is missing/unparseable (malformed or absent file).
fn mem_info_from_proc_meminfo(text: &str) -> Option<MemInfo> {
    let total_kb = parse_meminfo_kb(text, "MemTotal")?;
    let available_kb = parse_meminfo_kb(text, "MemAvailable")?;
    let total_mb = total_kb / 1024;
    let available_mb = available_kb / 1024;
    Some(MemInfo {
        total_mb,
        available_mb,
        used_mb: total_mb.saturating_sub(available_mb),
    })
}

fn read_mem_info() -> Option<MemInfo> {
    let text = std::fs::read_to_string("/proc/meminfo").ok()?;
    mem_info_from_proc_meminfo(&text)
}

// ── Uptime (/proc/uptime) ───────────────────────────────────────────────

/// Pure: parse `/proc/uptime`'s first field (seconds since boot, as a
/// float) into whole seconds.
fn parse_proc_uptime(text: &str) -> Option<u64> {
    let first = text.split_whitespace().next()?;
    first.parse::<f64>().ok().map(|s| s.max(0.0) as u64)
}

fn read_uptime_secs() -> Option<u64> {
    let text = std::fs::read_to_string("/proc/uptime").ok()?;
    parse_proc_uptime(&text)
}

// ── Load average (/proc/loadavg) ────────────────────────────────────────

/// Pure: parse `/proc/loadavg`'s first three whitespace-separated fields.
fn parse_loadavg(text: &str) -> Option<LoadAverage> {
    let mut parts = text.split_whitespace();
    let load1 = parts.next()?.parse::<f64>().ok()?;
    let load5 = parts.next()?.parse::<f64>().ok()?;
    let load15 = parts.next()?.parse::<f64>().ok()?;
    Some(LoadAverage { load1, load5, load15 })
}

fn read_load_average() -> Option<LoadAverage> {
    let text = std::fs::read_to_string("/proc/loadavg").ok()?;
    parse_loadavg(&text)
}

// ── Temperature (/sys/class/thermal/thermal_zone*/temp) ────────────────

/// Pure: `/sys/class/thermal/thermal_zone*/temp` reports milli-Celsius as a
/// bare integer (e.g. `"45231\n"`). `None` on anything that doesn't parse.
fn parse_thermal_millicelsius(text: &str) -> Option<f64> {
    text.trim().parse::<i64>().ok().map(|m| m as f64 / 1000.0)
}

/// First readable thermal zone, in ascending `thermal_zoneN` order. Debian
/// appliance hardware typically exposes at least one (`x86_pkg_temp` /
/// `acpitz`); this deliberately doesn't try to pick "the right" zone by
/// `type` — any single reading is enough for a dashboard health glance, and
/// `device.status`'s job is a snapshot, not thermal diagnostics.
fn read_temperature_c() -> Option<f64> {
    let mut zones: Vec<_> = std::fs::read_dir("/sys/class/thermal")
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("thermal_zone"))
        })
        .collect();
    zones.sort();
    for zone in zones {
        if let Ok(text) = std::fs::read_to_string(zone.join("temp"))
            && let Some(c) = parse_thermal_millicelsius(&text)
        {
            return Some(c);
        }
    }
    None
}

// ── Disk usage (statvfs) ────────────────────────────────────────────────

/// Pure: compute MB-rounded totals from raw `statvfs` block counts.
/// `used_mb` is total-minus-free (not total-minus-available — matches `df`
/// without the `-P`/reserved-block distinction, a reasonable simplification
/// for a dashboard glance).
fn disk_usage_from_blocks(block_size: u64, blocks: u64, blocks_free: u64, blocks_available: u64) -> DiskUsage {
    const MB: u64 = 1024 * 1024;
    let total_mb = blocks.saturating_mul(block_size) / MB;
    let free_mb = blocks_free.saturating_mul(block_size) / MB;
    let available_mb = blocks_available.saturating_mul(block_size) / MB;
    DiskUsage {
        total_mb,
        used_mb: total_mb.saturating_sub(free_mb),
        available_mb,
    }
}

#[cfg(unix)]
fn read_disk_usage(path: &Path) -> Option<DiskUsage> {
    let stat = nix::sys::statvfs::statvfs(path).ok()?;
    Some(disk_usage_from_blocks(
        stat.block_size() as u64,
        stat.blocks() as u64,
        stat.blocks_free() as u64,
        stat.blocks_available() as u64,
    ))
}

#[cfg(not(unix))]
fn read_disk_usage(_path: &Path) -> Option<DiskUsage> {
    None
}

// ── Network interfaces (getifaddrs) ─────────────────────────────────────

/// One (interface, address) row as extracted from a raw `getifaddrs` entry
/// — separated from the syscall itself so grouping/formatting is pure and
/// testable with synthetic input.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RawIfaceAddr {
    name: String,
    is_up: bool,
    /// `None` for address families we don't render (link-layer/packet
    /// entries `getifaddrs` also returns on Linux).
    ip: Option<String>,
}

/// Pure: group raw (interface, address) rows into one entry per interface
/// name, deduping repeated addresses and OR-ing `is_up` across rows for the
/// same interface (every row for one interface carries the same flags in
/// practice, but OR is the safe combinator if that ever isn't true).
fn group_interfaces(raw: Vec<RawIfaceAddr>) -> Vec<NetworkInterface> {
    let mut out: Vec<NetworkInterface> = Vec::new();
    for row in raw {
        if let Some(existing) = out.iter_mut().find(|i| i.name == row.name) {
            existing.is_up |= row.is_up;
            if let Some(ip) = row.ip
                && !existing.addresses.contains(&ip)
            {
                existing.addresses.push(ip);
            }
        } else {
            out.push(NetworkInterface {
                name: row.name.clone(),
                is_up: row.is_up,
                addresses: row.ip.into_iter().collect(),
            });
        }
    }
    out
}

#[cfg(unix)]
fn read_network_interfaces() -> Vec<NetworkInterface> {
    let Ok(addrs) = nix::ifaddrs::getifaddrs() else {
        return Vec::new();
    };
    let raw: Vec<RawIfaceAddr> = addrs
        .map(|a| {
            let is_up = a.flags.contains(nix::net::if_::InterfaceFlags::IFF_UP);
            let ip = a.address.as_ref().and_then(|addr| {
                addr.as_sockaddr_in()
                    .map(|v4| v4.ip().to_string())
                    .or_else(|| addr.as_sockaddr_in6().map(|v6| v6.ip().to_string()))
            });
            RawIfaceAddr { name: a.interface_name, is_up, ip }
        })
        .collect();
    group_interfaces(raw)
}

#[cfg(not(unix))]
fn read_network_interfaces() -> Vec<NetworkInterface> {
    Vec::new()
}

// ── Orchestration ────────────────────────────────────────────────────────

/// Collect a full `device.status` snapshot. `home_dir` is stat'd for disk
/// usage — on the appliance image that's `/data/duduclaw`, which
/// `statvfs()` reports the same filesystem-wide totals for as `/data`
/// itself (statvfs reports the mount containing the path, not just the
/// subdirectory), so this doesn't need to separately resolve `/data`.
pub fn collect_status(home_dir: &Path) -> DeviceStatus {
    DeviceStatus {
        cpu_cores: std::thread::available_parallelism().map(|p| p.get()).unwrap_or(1),
        load_average: read_load_average(),
        ram: read_mem_info(),
        disk: read_disk_usage(home_dir),
        temperature_c: read_temperature_c(),
        uptime_secs: read_uptime_secs(),
        network_interfaces: read_network_interfaces(),
    }
}

/// `device.network` read path — just the interface list, reusable
/// independent of the full status snapshot.
pub fn collect_network() -> Vec<NetworkInterface> {
    read_network_interfaces()
}

/// Does this `device.network` call's params ask to WRITE (set a static IP)
/// rather than just read? Pure — any of the network-config-shaped keys
/// being present is treated as a write attempt, regardless of value, so a
/// caller can't dodge the `not_implemented` refusal by sending a
/// technically-empty-but-present field.
pub fn is_network_write_request(params: &serde_json::Value) -> bool {
    const WRITE_KEYS: &[&str] = &["static_ip", "gateway", "netmask", "dns", "interface_config"];
    WRITE_KEYS.iter().any(|k| params.get(k).is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── /proc/meminfo ────────────────────────────────────────────────

    #[test]
    fn mem_info_parses_typical_proc_meminfo() {
        let text = "MemTotal:       16336452 kB\n\
                     MemFree:         1234567 kB\n\
                     MemAvailable:    8000000 kB\n";
        let info = mem_info_from_proc_meminfo(text).unwrap();
        assert_eq!(info.total_mb, 16336452 / 1024);
        assert_eq!(info.available_mb, 8000000 / 1024);
        assert_eq!(info.used_mb, info.total_mb - info.available_mb);
    }

    #[test]
    fn mem_info_missing_field_is_none() {
        assert!(mem_info_from_proc_meminfo("MemTotal: 1000 kB\n").is_none());
        assert!(mem_info_from_proc_meminfo("").is_none());
    }

    // ── /proc/uptime ─────────────────────────────────────────────────

    #[test]
    fn uptime_parses_first_field_floored() {
        assert_eq!(parse_proc_uptime("12345.67 98765.43\n"), Some(12345));
    }

    #[test]
    fn uptime_malformed_is_none() {
        assert_eq!(parse_proc_uptime(""), None);
        assert_eq!(parse_proc_uptime("not-a-number 1.0"), None);
    }

    // ── /proc/loadavg ────────────────────────────────────────────────

    #[test]
    fn loadavg_parses_first_three_fields() {
        let la = parse_loadavg("0.10 0.25 0.30 1/234 5678\n").unwrap();
        assert_eq!(la, LoadAverage { load1: 0.10, load5: 0.25, load15: 0.30 });
    }

    #[test]
    fn loadavg_short_line_is_none() {
        assert_eq!(parse_loadavg("0.10 0.25"), None);
        assert_eq!(parse_loadavg(""), None);
    }

    // ── thermal ──────────────────────────────────────────────────────

    #[test]
    fn thermal_parses_millicelsius() {
        assert_eq!(parse_thermal_millicelsius("45231\n"), Some(45.231));
        assert_eq!(parse_thermal_millicelsius("0"), Some(0.0));
    }

    #[test]
    fn thermal_malformed_is_none() {
        assert_eq!(parse_thermal_millicelsius("not-a-number"), None);
        assert_eq!(parse_thermal_millicelsius(""), None);
    }

    // ── disk usage ───────────────────────────────────────────────────

    #[test]
    fn disk_usage_computes_mb_from_blocks() {
        // 4096-byte blocks, 1,000,000 total, 250,000 free, 200,000 available.
        let usage = disk_usage_from_blocks(4096, 1_000_000, 250_000, 200_000);
        let mb = |blocks: u64| blocks * 4096 / (1024 * 1024);
        assert_eq!(usage.total_mb, mb(1_000_000));
        assert_eq!(usage.available_mb, mb(200_000));
        assert_eq!(usage.used_mb, mb(1_000_000) - mb(250_000));
    }

    #[test]
    fn disk_usage_saturates_instead_of_overflow_or_panic() {
        let usage = disk_usage_from_blocks(u64::MAX, u64::MAX, 0, 0);
        // Must not panic (saturating_mul); exact value isn't the point.
        assert!(usage.total_mb > 0);
    }

    #[test]
    fn disk_usage_real_statvfs_on_this_host_does_not_panic() {
        // Real syscall smoke test — nix's statvfs/fs feature is available
        // cross-Unix (not Linux-only), so this genuinely exercises the
        // syscall path on the macOS dev/CI host too, not just Linux.
        let usage = read_disk_usage(Path::new("/"));
        assert!(usage.is_some(), "statvfs(\"/\") should succeed on any Unix host");
    }

    // ── network interfaces (pure grouping) ──────────────────────────

    #[test]
    fn group_interfaces_dedups_and_collects_addresses() {
        let raw = vec![
            RawIfaceAddr { name: "eth0".into(), is_up: true, ip: Some("192.168.1.5".into()) },
            RawIfaceAddr { name: "eth0".into(), is_up: true, ip: Some("fe80::1".into()) },
            RawIfaceAddr { name: "eth0".into(), is_up: true, ip: Some("192.168.1.5".into()) }, // dup
            RawIfaceAddr { name: "lo".into(), is_up: true, ip: Some("127.0.0.1".into()) },
        ];
        let grouped = group_interfaces(raw);
        assert_eq!(grouped.len(), 2);
        let eth0 = grouped.iter().find(|i| i.name == "eth0").unwrap();
        assert_eq!(eth0.addresses, vec!["192.168.1.5", "fe80::1"]);
        assert!(eth0.is_up);
    }

    #[test]
    fn group_interfaces_keeps_interface_with_no_ip() {
        // e.g. a link-layer-only row (AF_PACKET) — interface still shows up,
        // just with an empty address list, rather than being dropped.
        let raw = vec![RawIfaceAddr { name: "eth1".into(), is_up: false, ip: None }];
        let grouped = group_interfaces(raw);
        assert_eq!(grouped.len(), 1);
        assert!(grouped[0].addresses.is_empty());
        assert!(!grouped[0].is_up);
    }

    #[test]
    fn network_interfaces_real_getifaddrs_on_this_host_does_not_panic() {
        // Real syscall smoke test on the dev/CI host — must return at least
        // loopback on any sane Unix host.
        let ifaces = read_network_interfaces();
        assert!(!ifaces.is_empty(), "expected at least loopback");
    }

    // ── device.network write-intent detection ───────────────────────

    #[test]
    fn network_write_request_detects_any_write_key() {
        assert!(is_network_write_request(&serde_json::json!({"static_ip": "10.0.0.5"})));
        assert!(is_network_write_request(&serde_json::json!({"gateway": null})));
        assert!(!is_network_write_request(&serde_json::json!({})));
        assert!(!is_network_write_request(&serde_json::json!({"unrelated": 1})));
    }

    // ── collect_status orchestration smoke test ─────────────────────

    #[tokio::test]
    async fn collect_status_never_panics_and_reports_at_least_cpu_cores() {
        let dir = tempfile::tempdir().unwrap();
        let status = collect_status(dir.path());
        assert!(status.cpu_cores >= 1);
        // Every other field is legitimately `None`/empty off-Linux or on a
        // host with no readable sensors — that's honest, not a bug.
    }
}
