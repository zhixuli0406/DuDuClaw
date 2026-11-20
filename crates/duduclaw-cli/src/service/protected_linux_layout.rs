//! Fixed installation inventory. Publication/activation belongs to signed bootstrap.
// Keep this module std-only so its policy tests can run without building the CLI.
pub(super) const BINARIES: &[&str] = &[
    "/usr/lib/duduclaw/duduclaw-spawnd",
    "/usr/lib/duduclaw/duduclaw-run",
    // R7a: the trampoline's isolation self-test (ExecStart of the fixed self-test unit).
    "/usr/lib/duduclaw/duduclaw-run-selftest",
    // R7c: the SSH denial probe (ExecStart of the probe template unit).
    "/usr/lib/duduclaw/duduclaw-isolation-probe",
    "/opt/duduclaw/bin/duduclaw-egress",
    "/usr/libexec/duduclaw/duduclaw-parse-worker",
    "/usr/lib/duduclaw/current/duduclaw-import",
    "/usr/lib/duduclaw/current/duduclaw-migrate",
    "/usr/lib/duduclaw/current/duduclaw-mcp-bridge",
    // The gateway copy G's `installed("duduclaw")` runs (duduclaw-migration
    // root_pipeline); the same signed artifact as /opt/duduclaw/bin/duduclaw.
    "/usr/lib/duduclaw/current/duduclaw",
    "/usr/libexec/duduclaw-updated-launch",
];

// `duduclaw-selftest` is `duduclaw_isolation::ids::SELFTEST_USER` (a literal:
// this module stays std-only; equality is tested in protected_linux_tests.rs).
pub(super) const ACCOUNTS: &[&str] = &[
    "duduclaw-runsup", "duduclaw-egress", "duduclaw-dl", "duduclaw-dns", "duduclaw-selftest",
];

// Parent-first; none of these directories belongs to an employee or gateway.
pub(super) const DIRECTORIES: &[(&str, u32)] = &[
    ("/usr/lib/duduclaw", 0o755),
    ("/usr/lib/duduclaw/parse-root", 0o755),
    ("/usr/lib/duduclaw/parse-root-tree", 0o755),
    // A real directory of file copies, never a symlink (plan §3.1).
    ("/usr/lib/duduclaw/current", 0o755),
    ("/usr/libexec/duduclaw", 0o755),
    ("/var/lib/duduclaw-spawnd", 0o700),
    ("/var/lib/duduclaw-spawnd/audit", 0o700),
    ("/var/lib/duduclaw-spawnd/etc", 0o700),
    ("/etc/duduclaw", 0o755),
    ("/etc/duduclaw-updated", 0o700),
    ("/var/lib/duduclaw-updated", 0o700),
    ("/opt/duduclaw-isolated", 0o755),
    ("/opt/duduclaw-isolated/generations", 0o755),
    ("/opt/duduclaw-isolated/slots", 0o700),
    ("/opt/duduclaw-isolated/slots/a", 0o700),
    ("/opt/duduclaw-isolated/slots/b", 0o700),
    ("/var/lib/duduclaw-migrate", 0o700),
];

pub(super) const UNITS: &[(&str, &str)] = &[
    ("duduclaw-spawnd.service", include_str!("../../../../scripts/linux-protected/spawnd/duduclaw-spawnd.service")),
    ("duduclaw-spawnd.socket", include_str!("../../../../scripts/linux-protected/spawnd/duduclaw-spawnd.socket")),
    ("duduclaw-isolation-ready.service", include_str!("../../../../scripts/linux-protected/spawnd/duduclaw-isolation-ready.service")),
    // R7a: no [Install]; never enabled or started here (spawnd starts it through PID 1).
    ("duduclaw-isolation-selftest.service", include_str!("../../../../scripts/linux-protected/spawnd/duduclaw-isolation-selftest.service")),
    // R7c: template, no [Install]; never enabled or started here (spawnd starts
    // `duduclaw-isolation-probe@<account>.service` through PID 1).
    ("duduclaw-isolation-probe@.service", include_str!("../../../../scripts/linux-protected/spawnd/duduclaw-isolation-probe@.service")),
    ("duduclaw.slice", include_str!("../../../../scripts/linux-protected/spawnd/duduclaw.slice")),
    ("duduclaw-tcb.slice", include_str!("../../../../scripts/linux-protected/spawnd/duduclaw-tcb.slice")),
    ("duduclaw-employees.slice", include_str!("../../../../scripts/linux-protected/spawnd/duduclaw-employees.slice")),
    ("duduclaw-egress.socket", include_str!("../../../duduclaw-egress/systemd/duduclaw-egress.socket")),
    ("duduclaw-egress.service", include_str!("../../../duduclaw-egress/systemd/duduclaw-egress.service")),
    ("duduclaw-dns-broker.socket", include_str!("../../../duduclaw-egress/systemd/duduclaw-dns-broker.socket")),
    // Written as `dns_broker_unit_text` generates it for the install's route.
    (DNS_BROKER_UNIT, DNS_BROKER_UNIT_TEXT),
    ("duduclaw-parse.socket", include_str!("../../../duduclaw-parse-worker/systemd/duduclaw-parse.socket")),
    ("duduclaw-parse@.service", include_str!("../../../duduclaw-parse-worker/systemd/duduclaw-parse@.service")),
    ("duduclaw-updated.service", include_str!("../../../../scripts/linux-protected/updated/duduclaw-updated.service")),
    ("duduclaw-import@.service", include_str!("../../../duduclaw-import/systemd/duduclaw-import@.service")),
    ("duduclaw-migrate-expire@.service", include_str!("../../../duduclaw-migration/systemd/duduclaw-migrate-expire@.service")),
    ("duduclaw-migrate-expire@.timer", include_str!("../../../duduclaw-migration/systemd/duduclaw-migrate-expire@.timer")),
];

/// The DNS broker unit: the one layout unit whose installed text depends on
/// the install (its upstream's network rule).
pub(super) const DNS_BROKER_UNIT: &str = "duduclaw-dns-broker.service";
/// As shipped: deny-all, no allow line (inert).
pub(super) const DNS_BROKER_UNIT_TEXT: &str = include_str!("../../../duduclaw-egress/systemd/duduclaw-dns-broker.service");
/// The shipped broker unit's one comment line the installer replaces with the
/// upstream's `IPAddressAllow=` line.
pub(super) const DNS_BROKER_ALLOW_MARKER: &str = "# @DUDUCLAW-DNS-UPSTREAM-ALLOW@";
/// Start of every broker unit mismatch message ([`dns_broker_unit_mismatch`]).
pub(super) const DNS_BROKER_MISMATCH_HEAD: &str = "DNS 轉送服務單元 /etc/systemd/system/duduclaw-dns-broker.service";

/// The refusal of an IPv4 link-local DNS upstream (169.254.0.0/16), with
/// what to do instead.
pub(super) const LINK_LOCAL_UPSTREAM_REFUSED: &str = "DNS 上游不能是 IPv4 鏈路本地位址（169.254.0.0/16，例如雲端主機的中繼資料服務位址）：DNS 轉送服務的防火牆規則只限制主機、不限制連接埠，轉送服務因此也能連到該位址上的其他服務（例如雲端憑證）。如果這台主機唯一的 DNS 解析器是鏈路本地位址（雲端虛擬機常見），請安裝並啟用 systemd-resolved，安裝程式會直接使用它，不需要 --dns-upstream；或以 --dns-upstream 指定一個位址可路由的 DNS 解析器。";

/// `upstream` is an IPv4 link-local address (169.254.0.0/16).
pub(super) fn ipv4_link_local(upstream: std::net::SocketAddr) -> bool {
    matches!(upstream.ip(), std::net::IpAddr::V4(ip) if ip.is_link_local())
}

/// The broker upstream's network rule: `IPAddressAllow=<ip>/32` (IPv4) or
/// `IPAddressAllow=<ip>/128` (IPv6, as `Ipv6Addr` displays it). Refuses what
/// the broker's own rule refuses (`duduclaw_egress::resolver::validate_broker_upstream`:
/// port 53, not unspecified, multicast or the IPv4 broadcast address; restated
/// because this module is std-only, and the installer applies that function
/// too) and what the rule cannot carry: a loopback host (spawnd's readback
/// refuses it; it would open every local service to the broker), an
/// IPv4-mapped IPv6 address (its traffic is IPv4, the /128 rule would not
/// match it), an IPv6 link-local address (the rule has no zone) and an IPv4
/// link-local address (169.254.0.0/16, [`LINK_LOCAL_UPSTREAM_REFUSED`]): the
/// rule limits the host, not the port, so a compromised broker could reach a
/// link-local service such as the cloud metadata endpoint's HTTP port.
pub(super) fn dns_broker_allow_line(upstream: std::net::SocketAddr) -> Result<String, &'static str> {
    use std::net::IpAddr;
    const REFUSED: &str = "DNS 上游必須是單一、非本機迴路的單點傳播 IP 位址加上連接埠 53。";
    if ipv4_link_local(upstream) { return Err(LINK_LOCAL_UPSTREAM_REFUSED); }
    if upstream.port() != 53 { return Err(REFUSED); }
    match upstream.ip() {
        IpAddr::V4(ip) if ip.is_unspecified() || ip.is_loopback() || ip.is_multicast() || ip.is_broadcast() => Err(REFUSED),
        IpAddr::V4(ip) => Ok(format!("IPAddressAllow={ip}/32")),
        IpAddr::V6(ip) if ip.is_unspecified() || ip.is_loopback() || ip.is_multicast()
            || ip.to_ipv4_mapped().is_some() || ip.segments()[0] & 0xffc0 == 0xfe80 => Err(REFUSED),
        IpAddr::V6(ip) => Ok(format!("IPAddressAllow={ip}/128")),
    }
}

/// The broker unit text the installer writes and every check expects:
/// `shipped` unchanged on the resolved route (`upstream` `None`); on the
/// broker route `shipped` with its marker line replaced by exactly one
/// [`dns_broker_allow_line`]. Refuses unless the marker is exactly one line
/// and `shipped` has no allow line of its own.
pub(super) fn dns_broker_unit_text(shipped: &str, upstream: Option<std::net::SocketAddr>) -> Result<String, &'static str> {
    if shipped.split('\n').filter(|line| *line == DNS_BROKER_ALLOW_MARKER).count() != 1
        || shipped.lines().any(|line| line.trim_start().starts_with("IPAddressAllow")) {
        return Err("DNS 轉送服務單元範本的上游規則標記不是剛好一行；安裝版本可能已損壞。");
    }
    let Some(upstream) = upstream else { return Ok(shipped.to_owned()) };
    let allow = dns_broker_allow_line(upstream)?;
    Ok(shipped.split('\n').map(|line| if line == DNS_BROKER_ALLOW_MARKER { allow.as_str() } else { line })
        .collect::<Vec<_>>().join("\n"))
}

/// The text the installer writes for layout unit `name` (`shipped`, except
/// the broker unit: [`dns_broker_unit_text`]).
pub(super) fn installed_unit_text(name: &str, shipped: &str, upstream: Option<std::net::SocketAddr>)
    -> Result<String, &'static str> {
    if name == DNS_BROKER_UNIT { dns_broker_unit_text(shipped, upstream) } else { Ok(shipped.to_owned()) }
}

/// The installed broker unit bytes are exactly the text for `upstream` (this
/// install's, `None` on the resolved route). Otherwise the error names the
/// expected and the found allow rule ([`dns_broker_unit_mismatch`]).
pub(super) fn dns_broker_unit_check(installed: &[u8], upstream: Option<std::net::SocketAddr>) -> Result<(), String> {
    let expected = dns_broker_unit_text(DNS_BROKER_UNIT_TEXT, upstream).map_err(str::to_owned)?;
    if installed == expected.as_bytes() { return Ok(()); }
    Err(dns_broker_unit_mismatch(installed, upstream))
}

/// What differs, never the file: the expected allow line (and upstream) and
/// at most three found allow lines, each bounded and printable ASCII only.
pub(super) fn dns_broker_unit_mismatch(installed: &[u8], upstream: Option<std::net::SocketAddr>) -> String {
    let text = String::from_utf8_lossy(installed);
    let found: Vec<String> = text.lines().filter(|line| line.trim_start().starts_with("IPAddressAllow"))
        .map(|line| line.chars().take(80).map(|c| if c.is_ascii_graphic() || c == ' ' { c } else { '?' }).collect())
        .collect();
    let (wanted, wanted_lines) = match upstream.map(|upstream| (upstream, dns_broker_allow_line(upstream))) {
        Some((upstream, Ok(line))) => (format!("{line}（上游 {upstream}）"), Some(vec![line])),
        Some((upstream, Err(_))) => (format!("（無法產生：記錄的上游 {upstream} 不是可用的 DNS 上游）"), None),
        None => ("沒有 IPAddressAllow（使用 systemd-resolved 的主機）".to_owned(), Some(Vec::new())),
    };
    if wanted_lines.as_ref() == Some(&found) {
        return format!("{DNS_BROKER_MISMATCH_HEAD} 的上游規則相符，但其他內容與安裝版本不符。");
    }
    let shown = match found.len() {
        0 => "沒有 IPAddressAllow".to_owned(),
        count if count > 3 => format!("{} 等 {count} 行", found[..3].join("、")),
        _ => found.join("、"),
    };
    format!("{DNS_BROKER_MISMATCH_HEAD} 的上游規則不符：預期 {wanted}，實際為 {shown}。不要手動修改這個單元或加入覆寫設定；要更換上游請重新執行安裝程式。")
}

fn login_defs_value(defs: &str, key: &str) -> Result<Option<u32>, &'static str> {
    let mut found = None;
    for line in defs.lines() {
        let fields: Vec<_> = line.split('#').next().unwrap_or("").split_whitespace().collect();
        if fields.first().copied() == Some(key) {
            if fields.len() != 2 || found.is_some() { return Err("ambiguous system ID range"); }
            found = Some(fields[1].parse::<u32>().map_err(|_| "invalid system ID range")?);
        }
    }
    Ok(found)
}

pub(super) fn system_id_limit(defs: &str, key: &str) -> Result<u32, &'static str> {
    let minimum = match key {
        "SYS_UID_MAX" => "UID_MIN",
        "SYS_GID_MAX" => "GID_MIN",
        _ => return Err("unknown system ID limit"),
    };
    // Like shadow useradd/groupadd -r, an absent maximum defaults to login min - 1.
    let value = match login_defs_value(defs, key)? {
        Some(value) => value,
        None => login_defs_value(defs, minimum)?.unwrap_or(1000).checked_sub(1)
            .ok_or("invalid system ID range")?,
    };
    if value > 1 && value < 61184 { Ok(value) }
    else { Err("invalid system ID range in /etc/login.defs") }
}

pub(super) fn validate_system_ids(uid: u32, gid: u32, defs: &str) -> Result<(), &'static str> {
    let uid_max = system_id_limit(defs, "SYS_UID_MAX")?;
    let gid_max = system_id_limit(defs, "SYS_GID_MAX")?;
    if uid == 0 || gid == 0 || uid >= uid_max || gid >= gid_max {
        return Err("service identities must be below the system ID limits");
    }
    Ok(())
}

// G retains a root-owned enclosing directory so the gateway cannot replace
// its own home. Production callers always pass root ownership as 0:0.
#[cfg(unix)]
pub(super) fn migrated_home_is_private(parent: &std::path::Path, root_uid: u32, root_gid: u32,
    gateway_uid: u32, gateway_gid: u32) -> std::io::Result<bool> {
    use std::{fs, os::unix::fs::MetadataExt};
    for (path, uid, gid, mode) in [(parent.to_path_buf(), root_uid, root_gid, 0o711),
        (parent.join(".duduclaw"), gateway_uid, gateway_gid, 0o700)] {
        let metadata = fs::symlink_metadata(path)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() || metadata.uid() != uid
            || metadata.gid() != gid || metadata.mode() & 0o7777 != mode { return Ok(false); }
    }
    Ok(true)
}

// systemd leaves empty mountpoints after a worker exits. Only this bounded
// skeleton may survive a reinstall; never delete unexpected content recursively.
#[cfg(unix)]
pub(super) fn parse_root_is_reusable(path: &std::path::Path, uid: u32, gid: u32) -> std::io::Result<bool> {
    use std::{fs, os::unix::fs::MetadataExt};
    let base = fs::symlink_metadata(path)?;
    if !base.is_dir() || base.file_type().is_symlink() || base.uid() != uid || base.gid() != gid
        || base.mode() & 0o022 != 0 { return Ok(false); }
    let allowed = [".tmp", "dev", "etc", "home", "lib", "lib64", "proc", "root", "run", "sys", "tmp", "usr", "var", "var/tmp"];
    let mut pending = vec![path.to_path_buf()];
    let mut seen = 0;
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            seen += 1;
            if seen > allowed.len() { return Ok(false); }
            let child = entry.path();
            let relative = child.strip_prefix(path).ok().and_then(|value| value.to_str());
            if !relative.is_some_and(|value| allowed.contains(&value)) { return Ok(false); }
            let meta = fs::symlink_metadata(&child)?;
            if !meta.is_dir() || meta.file_type().is_symlink() || meta.uid() != uid || meta.gid() != gid
                || meta.dev() != base.dev() || meta.mode() & 0o022 != 0 { return Ok(false); }
            pending.push(child);
        }
    }
    Ok(true)
}

/// R8s: the parse runtime tree's mount points, read from their one
/// definition: the `MOUNT_POINTS = (…)` line of the embedded tree
/// builder/verifier (`build-runtime-tree.py`, which the installer and every
/// readiness/acceptance check run). Test-only: nothing in Rust builds or
/// walks the tree.
#[cfg(test)]
pub(super) fn parse_runtime_mount_points() -> Vec<&'static str> {
    const BUILDER: &str = include_str!("../../../duduclaw-parse-worker/scripts/build-runtime-tree.py");
    let lines: Vec<&'static str> = BUILDER.lines().filter(|line| line.starts_with("MOUNT_POINTS")).collect();
    assert_eq!(lines.len(), 1, "exactly one MOUNT_POINTS line");
    let items = lines[0].strip_prefix("MOUNT_POINTS = (").and_then(|rest| rest.strip_suffix(')'))
        .expect("MOUNT_POINTS = ('…', …) on one line");
    items.split(',').map(str::trim).filter(|item| !item.is_empty())
        .map(|item| item.strip_prefix('\'').and_then(|item| item.strip_suffix('\'')).expect("quoted mount point"))
        .collect()
}

// The caller fixes root to / and expected ownership to 0:0. Tests use an
// isolated tree; there is no environment-controlled production destination.
#[cfg(unix)]
pub(super) fn published_components(root: &std::path::Path, uid: u32, gid: u32)
    -> std::io::Result<bool> {
    use std::{fs, io::{Error, ErrorKind}, os::unix::fs::MetadataExt};
    let unsafe_path = || Error::new(ErrorKind::PermissionDenied, "unsafe or incomplete isolation publication");
    let paths: Vec<_> = BINARIES.iter().map(|path| root.join(path.trim_start_matches('/'))).collect();
    let present: Vec<_> = paths.iter().map(fs::symlink_metadata).collect();
    if present.iter().all(|entry| entry.as_ref().is_err_and(|error| error.kind() == ErrorKind::NotFound)) {
        return Ok(false);
    }
    for (path, entry) in paths.iter().zip(present) {
        let meta = entry?;
        if !meta.is_file() || meta.file_type().is_symlink() || meta.uid() != uid || meta.gid() != gid
            || meta.mode() & 0o7777 != 0o755 || meta.nlink() != 1 || meta.len() == 0 {
            return Err(unsafe_path());
        }
        for parent in path.parent().ok_or_else(unsafe_path)?.ancestors() {
            let meta = fs::symlink_metadata(parent)?;
            if !meta.is_dir() || meta.file_type().is_symlink() || meta.uid() != uid || meta.gid() != gid
                || meta.mode() & 0o022 != 0 { return Err(unsafe_path()); }
            if parent == root { break; }
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_protected_layout_rejects_unsafe_or_unknown_system_ranges() {
        let defs = "SYS_UID_MAX 999\nSYS_GID_MAX 999 # explicit\n";
        assert!(validate_system_ids(998, 998, defs).is_ok());
        for id in [0, 999, 1000, 61184, 65519, 70000, 99999] {
            assert!(validate_system_ids(id, 998, defs).is_err());
            assert!(validate_system_ids(998, id, defs).is_err());
        }
        for defs in ["SYS_UID_MAX 999\nSYS_UID_MAX 998\nSYS_GID_MAX 999",
            "SYS_UID_MAX 70000\nSYS_GID_MAX 999", "SYS_UID_MAX 999 extra\nSYS_GID_MAX 999"] {
            assert!(validate_system_ids(500, 500, defs).is_err());
        }
    }

    #[test]
    fn service_protected_layout_uses_shadow_system_id_defaults() {
        let stock = "UID_MIN 1000\nUID_MAX 60000\n#SYS_UID_MIN 100\n#SYS_UID_MAX 999\nGID_MIN 1000\nGID_MAX 60000\n#SYS_GID_MIN 100\n#SYS_GID_MAX 999\n";
        for (key, minimum) in [("SYS_UID_MAX", "UID_MIN"), ("SYS_GID_MAX", "GID_MIN")] {
            assert_eq!(system_id_limit(stock, key), Ok(999));
            assert_eq!(system_id_limit("", key), Ok(999));
            assert_eq!(system_id_limit(&format!("{minimum} 500\n"), key), Ok(499));
            assert_eq!(system_id_limit(&format!("{minimum} invalid\n{key} 800 # explicit\n"), key), Ok(800));
            for comment in [format!("# {key} 999\n"), format!("#{key}\t999\n")] {
                assert_eq!(system_id_limit(&comment, key), Ok(999));
            }
            for name in [key, minimum] {
                for value in ["", "invalid", "999 extra", "70000", "0", "1", "4294967296"] {
                    assert!(system_id_limit(&format!("{name} {value}\n"), key).is_err());
                }
                assert!(system_id_limit(&format!("{name} 999\n{name} 999\n"), key).is_err());
            }
            assert_eq!(system_id_limit(&format!("{key} 2\n"), key), Ok(2));
            assert!(system_id_limit(&format!("{minimum} 2\n"), key).is_err());
            assert!(system_id_limit(&format!("{key} 61184\n"), key).is_err());
            assert_eq!(system_id_limit(&format!("{key} 61183\n"), key), Ok(61183));
            assert_eq!(system_id_limit(&format!("{minimum} 61184\n"), key), Ok(61183));
            assert!(system_id_limit(&format!("{minimum} 61185\n"), key).is_err());
        }
        assert!(validate_system_ids(500, 500, "").is_ok());
        assert!(validate_system_ids(500, 500, "# SYS_UID_MAX 999\nSYS_GID_MAX 999").is_ok());
    }

    #[test]
    fn service_protected_layout_covers_unit_executables_and_empty_parse_roots() {
        let spawnd = UNITS.iter().find(|(name, _)| *name == "duduclaw-spawnd.service").unwrap().1;
        assert_eq!(spawnd, include_str!("../../../../scripts/linux-protected/spawnd/duduclaw-spawnd.service"));
        assert_eq!(spawnd.lines().filter(|line| *line == "MountFlags=private").count(), 1);
        assert_eq!(spawnd.lines().filter(|line| line.starts_with("MountFlags=")).count(), 1);
        for (_, text) in UNITS {
            for command in text.lines().filter_map(|line| line.strip_prefix("ExecStart=")) {
                let binary = command.split_whitespace().next().unwrap();
                assert!(BINARIES.contains(&binary), "unpublished unit executable: {binary}");
            }
        }
        for root in ["/usr/lib/duduclaw/parse-root", "/usr/lib/duduclaw/parse-root-tree"] {
            assert!(DIRECTORIES.contains(&(root, 0o755)));
        }
        assert_eq!(ACCOUNTS.len(), 5);
        assert_eq!(BINARIES.len(), 11);
        // R7a: the self-test unit runs the published self-test binary; the account it drops to is created here.
        let selftest = UNITS.iter().find(|(name, _)| *name == "duduclaw-isolation-selftest.service").unwrap().1;
        assert!(selftest.lines().any(|line| line == "ExecStart=/usr/lib/duduclaw/duduclaw-run-selftest"));
        assert!(!selftest.lines().any(|line| line.trim() == "[Install]"), "never enabled");
        assert!(ACCOUNTS.contains(&"duduclaw-selftest"));
        // R7c: the probe template runs the published probe binary unprivileged as duduclaw-selftest (no account of its own).
        let probe = UNITS.iter().find(|(name, _)| *name == "duduclaw-isolation-probe@.service").unwrap().1;
        assert!(probe.lines().any(|line| line == "ExecStart=/usr/lib/duduclaw/duduclaw-isolation-probe %i"));
        assert!(!probe.lines().any(|line| line.trim() == "[Install]"), "never enabled");
        // Every binary's parent directory is created by the layout (parent first).
        for binary in BINARIES {
            let parent = std::path::Path::new(binary).parent().unwrap().to_str().unwrap();
            assert!(DIRECTORIES.iter().any(|(path, mode)| *path == parent && *mode == 0o755)
                || matches!(parent, "/opt/duduclaw/bin" | "/usr/libexec"), "{binary}");
        }
        for (index, (path, _)) in DIRECTORIES.iter().enumerate() {
            let parent = std::path::Path::new(path).parent().unwrap().to_str().unwrap();
            if let Some(position) = DIRECTORIES.iter().position(|(other, _)| *other == parent) {
                assert!(position < index, "{path} listed before its parent");
            }
        }
    }

    /// R8s: the directories PID 1 needs (and would otherwise create) inside
    /// the parse runtime tree when it starts `duduclaw-parse@.service`,
    /// derived from the unit's `[Service]` directives: the tree is bound at the
    /// worker's `/` (`RootDirectory=` + `BindReadOnlyPaths=<tree>:/`), so every
    /// later mount target's missing path is created in the tree. Over-
    /// approximates (a directive that may mount under a path counts); a target
    /// below another mount target (`/dev/shm` in the private `/dev`) lives on
    /// that mount, not in the tree. `Err` names a directive this model does
    /// not cover: decide whether it needs a mount point in the tree first.
    fn parse_unit_tree_mount_points(unit: &str) -> Result<std::collections::BTreeSet<String>, String> {
        use std::collections::BTreeSet;
        const TREE: &str = "/usr/lib/duduclaw/parse-root-tree";
        const ROOT: &str = "/usr/lib/duduclaw/parse-root";
        // Directives that mount nothing in the worker's mount namespace.
        const NO_MOUNT: &[&str] = &["Type", "ExecStart", "StandardInput", "StandardOutput", "StandardError",
            "IPAddressDeny", "RestrictNamespaces", "RestrictSUIDSGID", "RestrictRealtime", "LockPersonality",
            "NoNewPrivileges", "CapabilityBoundingSet", "AmbientCapabilities", "RestrictAddressFamilies",
            "SystemCallArchitectures", "SystemCallFilter", "SystemCallErrorNumber", "MemoryMax", "MemorySwapMax",
            "CPUQuota", "TasksMax", "LimitNOFILE", "LimitCORE", "LimitFSIZE", "RuntimeMaxSec", "TimeoutStartSec",
            "TimeoutStopSec", "KillMode", "SendSIGKILL", "UMask"];
        fn flag(key: &str, value: &str) -> Result<bool, String> {
            match value {
                "yes" | "true" | "on" | "1" => Ok(true),
                "no" | "false" | "off" | "0" => Ok(false),
                _ => Err(format!("{key}={value}: not modelled")),
            }
        }
        fn add(targets: &mut Vec<String>, paths: &[&str]) { targets.extend(paths.iter().map(|path| (*path).to_owned())); }
        let mut targets: Vec<String> = Vec::new();
        let mut root = None;
        let mut tree_bound = 0;
        let mut sources_under_root: Vec<String> = Vec::new();
        let mut section = "";
        for line in unit.lines().map(str::trim) {
            if line.is_empty() || line.starts_with('#') || line.starts_with(';') { continue; }
            if line.starts_with('[') { section = line; continue; }
            if section != "[Service]" { continue; }
            let (key, value) = line.split_once('=').ok_or_else(|| format!("not a directive: {line}"))?;
            match key {
                _ if NO_MOUNT.contains(&key) => {}
                // PrivateTmp (DynamicUser forces it): /tmp and /var/tmp.
                "DynamicUser" | "PrivateTmp" => { if flag(key, value)? { add(&mut targets, &["/tmp", "/var/tmp"]); } }
                // A private /dev (PrivateIPC: its mqueue under /dev).
                "PrivateDevices" | "PrivateIPC" => { if flag(key, value)? { add(&mut targets, &["/dev"]); } }
                // A read-only /sys projection (sysfs for the private network, /sys/fs/cgroup).
                "PrivateNetwork" | "ProtectControlGroups" => { if flag(key, value)? { add(&mut targets, &["/sys"]); } }
                "ProtectKernelTunables" => { if flag(key, value)? { add(&mut targets, &["/proc", "/sys"]); } }
                // /proc/kmsg and /dev/kmsg.
                "ProtectKernelLogs" => { if flag(key, value)? { add(&mut targets, &["/proc", "/dev"]); } }
                "ProtectHostname" => { if flag(key, value)? { add(&mut targets, &["/proc"]); } }
                // Module directories made inaccessible only when present.
                "ProtectKernelModules" => { flag(key, value)?; }
                // A private procfs.
                "ProtectProc" => match value {
                    "default" => {}
                    "invisible" | "noaccess" | "ptraceable" => add(&mut targets, &["/proc"]),
                    _ => return Err(format!("{key}={value}: not modelled")),
                },
                "ProcSubset" => match value {
                    "all" => {}
                    "pid" => add(&mut targets, &["/proc"]),
                    _ => return Err(format!("{key}={value}: not modelled")),
                },
                "MountAPIVFS" => { if flag(key, value)? { add(&mut targets, &["/proc", "/sys", "/dev", "/run"]); } }
                // Read-only remounts of what exists (/usr, /boot, /efi skipped when absent).
                "ProtectSystem" if matches!(value, "strict" | "full" | "yes" | "true" | "no" | "false") => {}
                // /home, /root, /run/user made inaccessible only when present.
                "ProtectHome" => { flag(key, value)?; }
                "RootDirectory" => root = Some(value),
                "TemporaryFileSystem" => {
                    for item in value.split_whitespace() {
                        targets.push(item.split(':').next().unwrap_or_default().to_owned());
                    }
                }
                "BindPaths" | "BindReadOnlyPaths" => {
                    for item in value.split_whitespace() {
                        let mut fields = item.split(':');
                        let source = fields.next().unwrap_or_default().trim_start_matches('-');
                        let target = fields.next().unwrap_or(source);
                        if target == "/" {
                            if key != "BindReadOnlyPaths" || source != TREE {
                                return Err(format!("{key}={item}: only the runtime tree may be bound at /"));
                            }
                            tree_bound += 1;
                            continue;
                        }
                        if let Some(relative) = source.strip_prefix(ROOT).and_then(|rest| rest.strip_prefix('/')) {
                            sources_under_root.push(relative.to_owned());
                        }
                        targets.push(target.to_owned());
                    }
                }
                // Optional ('-') paths: applied only to what exists, never created.
                "InaccessiblePaths" | "ReadOnlyPaths" | "ReadWritePaths" | "ExecPaths" | "NoExecPaths" => {
                    for item in value.split_whitespace() {
                        if !item.starts_with('-') { return Err(format!("{key}={item}: not optional ('-')")); }
                    }
                }
                _ => return Err(format!("{key}=: not modelled; decide whether PID 1 creates a mount point for it in the tree")),
            }
        }
        if root != Some(ROOT) || tree_bound != 1 {
            return Err("the runtime tree must be bound exactly once at the unit's RootDirectory".to_owned());
        }
        let mounts: BTreeSet<String> = targets.iter().map(|target| target.trim_end_matches('/').to_owned()).collect();
        let mut required = BTreeSet::new();
        for target in &mounts {
            let parts: Vec<&str> = target.strip_prefix('/').ok_or_else(|| format!("unusual mount target {target:?}"))?
                .split('/').collect();
            if parts.iter().any(|part| part.is_empty() || *part == "." || *part == "..") {
                return Err(format!("unusual mount target {target:?}"));
            }
            for index in 0..parts.len() {
                let prefix = parts[..=index].join("/");
                // Below another mount target: created on that mount, not in the tree.
                let below_mount = index + 1 < parts.len() && mounts.contains(&format!("/{prefix}"));
                required.insert(prefix);
                if below_mount { break; }
            }
        }
        // A bind source inside the RootDirectory resolves through the tree
        // bound there (`parse-root/.tmp` is the `/.tmp` tmpfs).
        for relative in sources_under_root {
            if !mounts.contains(&format!("/{relative}")) {
                return Err(format!("bind source {ROOT}/{relative} is not a mount target in the tree"));
            }
        }
        Ok(required)
    }

    #[test]
    fn service_protected_layout_parse_tree_mount_points_derive_from_the_worker_unit() {
        use std::collections::BTreeSet;
        let unit = UNITS.iter().find(|(name, _)| *name == "duduclaw-parse@.service").unwrap().1;
        let list = parse_runtime_mount_points();
        // What one real start created on systemd 255 (R8s), parent first.
        assert_eq!(list, [".tmp", "dev", "proc", "run", "sys", "tmp", "var", "var/tmp"]);
        for (index, path) in list.iter().enumerate() {
            if let Some((parent, _)) = path.rsplit_once('/') {
                assert!(list[..index].iter().any(|candidate| *candidate == parent), "{path} listed before its parent");
            }
        }
        let listed: BTreeSet<String> = list.iter().map(|path| (*path).to_owned()).collect();
        assert_eq!(parse_unit_tree_mount_points(unit), Ok(listed.clone()));
        // A directive added or changed so that another mount point is needed
        // (or one is no longer accounted for) no longer matches the list.
        for (from, to) in [
            ("TemporaryFileSystem=/run:ro\n", "TemporaryFileSystem=/run:ro\nTemporaryFileSystem=/srv:ro\n"),
            ("BindPaths=/usr/lib/duduclaw/parse-root/.tmp:/var/tmp:norbind\n",
                "BindPaths=/usr/lib/duduclaw/parse-root/.tmp:/var/cache:norbind\n"),
            ("TemporaryFileSystem=/dev/shm:ro\n", "TemporaryFileSystem=/dev/shm:ro\nBindReadOnlyPaths=/etc/ssl:/etc/ssl\n"),
            ("TemporaryFileSystem=/.tmp:rw,nosuid,nodev,noexec,size=8M,nr_inodes=1024,mode=0700\n", ""),
            ("BindReadOnlyPaths=/usr/lib/duduclaw/parse-root-tree:/:norbind\n", ""),
            ("RootDirectory=/usr/lib/duduclaw/parse-root\n", ""),
        ] {
            assert!(unit.contains(from), "{from}");
            let changed = unit.replace(from, to);
            assert_ne!(parse_unit_tree_mount_points(&changed).ok(), Some(listed.clone()), "{to}");
        }
        // A mount-related directive this model does not cover refuses.
        for extra in ["RuntimeDirectory=parse", "StateDirectory=parse", "LoadCredential=key:/etc/key",
            "PrivateTmp=disconnected", "ProtectHome=tmpfs", "ProtectProc=other", "InaccessiblePaths=/usr/local",
            "TemporaryFileSystem=/", "BindPaths=/srv:/", "MountImages=/image:/mnt", "ExtensionDirectories=/ext"] {
            assert!(parse_unit_tree_mount_points(&format!("{unit}{extra}\n")).is_err(), "{extra}");
        }
    }

    #[test]
    fn service_protected_layout_dns_broker_unit_text_carries_exactly_the_upstream_allow_line() {
        let shipped = DNS_BROKER_UNIT_TEXT;
        // As shipped: deny-all, the marker one comment line, no allow line (inert).
        assert_eq!(UNITS.iter().filter(|(name, text)| *name == DNS_BROKER_UNIT && *text == shipped).count(), 1);
        assert!(shipped.lines().any(|line| line == "IPAddressDeny=any"));
        assert_eq!(shipped.lines().filter(|line| *line == DNS_BROKER_ALLOW_MARKER).count(), 1);
        assert!(!shipped.lines().any(|line| line.starts_with("IPAddressAllow")));
        // The resolved route: the shipped bytes.
        assert_eq!(dns_broker_unit_text(shipped, None).unwrap(), shipped);
        assert_eq!(installed_unit_text(DNS_BROKER_UNIT, shipped, None).unwrap(), shipped);
        for (upstream, line) in [("192.0.2.53:53", "IPAddressAllow=192.0.2.53/32"),
            ("10.0.0.2:53", "IPAddressAllow=10.0.0.2/32"),
            // Just outside 169.254.0.0/16.
            ("169.253.255.255:53", "IPAddressAllow=169.253.255.255/32"),
            ("169.255.0.1:53", "IPAddressAllow=169.255.0.1/32"),
            ("[2001:4860:4860::8888]:53", "IPAddressAllow=2001:4860:4860::8888/128"),
            ("[2001:DB8:0:0:0:0:0:35]:53", "IPAddressAllow=2001:db8::35/128"),
            ("[fd00:ec2::253]:53", "IPAddressAllow=fd00:ec2::253/128")] {
            let upstream: std::net::SocketAddr = upstream.parse().unwrap();
            assert_eq!(dns_broker_allow_line(upstream).unwrap(), line);
            let text = dns_broker_unit_text(shipped, Some(upstream)).unwrap();
            assert_eq!(installed_unit_text(DNS_BROKER_UNIT, shipped, Some(upstream)).unwrap(), text);
            assert!(text.lines().any(|candidate| candidate == "IPAddressDeny=any"));
            let allow: Vec<_> = text.lines().filter(|candidate| candidate.starts_with("IPAddressAllow")).collect();
            assert_eq!(allow, [line]);
            assert!(!text.contains(DNS_BROKER_ALLOW_MARKER));
            // No other difference: line by line, only the marker became the allow line.
            let (before, after): (Vec<_>, Vec<_>) = (shipped.split('\n').collect(), text.split('\n').collect());
            assert_eq!(before.len(), after.len());
            let changed: Vec<_> = before.iter().zip(&after).filter(|(old, new)| old != new).collect();
            assert_eq!(changed, [(&DNS_BROKER_ALLOW_MARKER, &line)]);
            assert_eq!(text.len(), shipped.len() - DNS_BROKER_ALLOW_MARKER.len() + line.len());
        }
        // The broker's own refusals, plus what the rule cannot carry.
        for bad in ["192.0.2.53:5353", "192.0.2.53:0", "0.0.0.0:53", "[::]:53", "224.0.0.1:53", "[ff02::1]:53",
            "255.255.255.255:53", "127.0.0.1:53", "127.0.0.53:53", "[::1]:53", "[::ffff:127.0.0.1]:53",
            "[::ffff:192.0.2.53]:53", "[fe80::1]:53"] {
            let upstream: std::net::SocketAddr = bad.parse().unwrap();
            assert!(dns_broker_allow_line(upstream).is_err(), "{bad}");
            assert!(dns_broker_unit_text(shipped, Some(upstream)).is_err(), "{bad}");
        }
        // IPv4 link-local (cloud metadata and resolver hosts included): the
        // rule limits the host, not the port. The refusal says what to do.
        for bad in ["169.254.169.254:53", "169.254.169.253:53", "169.254.0.1:53", "169.254.255.255:53", "169.254.169.254:80"] {
            let upstream: std::net::SocketAddr = bad.parse().unwrap();
            assert!(ipv4_link_local(upstream), "{bad}");
            assert_eq!(dns_broker_allow_line(upstream), Err(LINK_LOCAL_UPSTREAM_REFUSED), "{bad}");
            assert_eq!(dns_broker_unit_text(shipped, Some(upstream)), Err(LINK_LOCAL_UPSTREAM_REFUSED), "{bad}");
            assert!(installed_unit_text(DNS_BROKER_UNIT, shipped, Some(upstream)).is_err(), "{bad}");
        }
        assert!(LINK_LOCAL_UPSTREAM_REFUSED.contains("systemd-resolved") && LINK_LOCAL_UPSTREAM_REFUSED.contains("--dns-upstream"));
        assert!(!ipv4_link_local("192.0.2.53:53".parse().unwrap()) && !ipv4_link_local("[fe80::1]:53".parse().unwrap()));
        // The marker must be exactly one line and the template carry no allow line of its own.
        let upstream = Some("192.0.2.53:53".parse().unwrap());
        let damaged = [shipped.replace(&format!("{DNS_BROKER_ALLOW_MARKER}\n"), ""),
            shipped.replace(DNS_BROKER_ALLOW_MARKER, &format!("{DNS_BROKER_ALLOW_MARKER}\n{DNS_BROKER_ALLOW_MARKER}")),
            shipped.replace(DNS_BROKER_ALLOW_MARKER, &format!(" {DNS_BROKER_ALLOW_MARKER}")),
            shipped.replace("IPAddressDeny=any\n", "IPAddressDeny=any\nIPAddressAllow=192.0.2.53/32\n")];
        for text in &damaged {
            assert_ne!(text, shipped);
            assert!(dns_broker_unit_text(text, upstream).is_err());
            assert!(dns_broker_unit_text(text, None).is_err());
        }
        // Every other unit is written as shipped, whatever the route.
        for (name, text) in UNITS.iter().filter(|(name, _)| *name != DNS_BROKER_UNIT) {
            assert_eq!(installed_unit_text(name, text, upstream).unwrap(), *text);
            assert_eq!(installed_unit_text(name, text, None).unwrap(), *text);
        }
    }

    #[test]
    fn service_protected_layout_dns_broker_unit_check_expects_this_installs_upstream() {
        let upstream = Some("192.0.2.53:53".parse().unwrap());
        let generated = dns_broker_unit_text(DNS_BROKER_UNIT_TEXT, upstream).unwrap();
        assert_eq!(dns_broker_unit_check(generated.as_bytes(), upstream), Ok(()));
        assert_eq!(dns_broker_unit_check(DNS_BROKER_UNIT_TEXT.as_bytes(), None), Ok(()));
        // The shipped (deny-all) text on the broker route.
        let shipped = dns_broker_unit_check(DNS_BROKER_UNIT_TEXT.as_bytes(), upstream).unwrap_err();
        assert!(shipped.starts_with(DNS_BROKER_MISMATCH_HEAD), "{shipped}");
        assert!(shipped.contains("預期 IPAddressAllow=192.0.2.53/32（上游 192.0.2.53:53），實際為 沒有 IPAddressAllow。"), "{shipped}");
        // The generated text on the resolved route.
        let resolved = dns_broker_unit_check(generated.as_bytes(), None).unwrap_err();
        assert!(resolved.contains("預期 沒有 IPAddressAllow（使用 systemd-resolved 的主機），實際為 IPAddressAllow=192.0.2.53/32。"), "{resolved}");
        // A text generated for a different upstream.
        let foreign = dns_broker_unit_text(DNS_BROKER_UNIT_TEXT, Some("198.51.100.53:53".parse().unwrap())).unwrap();
        let other = dns_broker_unit_check(foreign.as_bytes(), upstream).unwrap_err();
        assert!(other.contains("預期 IPAddressAllow=192.0.2.53/32（上游 192.0.2.53:53），實際為 IPAddressAllow=198.51.100.53/32。"), "{other}");
        // The right allow line, other bytes changed.
        let edited = dns_broker_unit_check(generated.replace("MemoryMax=64M", "MemoryMax=1G").as_bytes(), upstream).unwrap_err();
        assert!(edited.contains("上游規則相符，但其他內容與安裝版本不符"), "{edited}");
        // An extra allow line: shown bounded and printable; never the file.
        let noisy = format!("{generated}IPAddressAllow=0.0.0.0/0\u{1b}[31m{}\n", "x".repeat(500));
        let extra = dns_broker_unit_check(noisy.as_bytes(), upstream).unwrap_err();
        assert!(extra.contains("實際為 IPAddressAllow=192.0.2.53/32、IPAddressAllow=0.0.0.0/0?[31m"), "{extra}");
        assert!(!extra.contains('\u{1b}') && !extra.contains(&"x".repeat(80)), "{extra}");
        for message in [&shipped, &resolved, &other, &edited, &extra] {
            assert!(!message.contains("ExecStart=") && !message.contains("IPAddressDeny") && message.len() < 1024, "{message}");
        }
        // A loopback upstream (the broker's own validator accepts it) never yields an expected text.
        assert!(dns_broker_unit_check(DNS_BROKER_UNIT_TEXT.as_bytes(), Some("127.0.0.1:53".parse().unwrap())).is_err());
    }
    #[cfg(unix)]
    struct Fixture(std::path::PathBuf);
    #[cfg(unix)]
    impl Fixture {
        fn new() -> Self {
            use std::{fs, os::unix::fs::PermissionsExt, sync::atomic::{AtomicU64, Ordering}, time::{SystemTime, UNIX_EPOCH}};
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let root = std::env::temp_dir().join(format!("iso-i2-layout-{}-{}-{}", std::process::id(),
                SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos(), NEXT.fetch_add(1, Ordering::Relaxed)));
            fs::create_dir(&root).unwrap();
            fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
            Self(root)
        }
        fn populate(&self) {
            use std::{fs, os::unix::fs::PermissionsExt};
            for path in BINARIES {
                let path = self.0.join(path.trim_start_matches('/'));
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(&path, b"fixture executable").unwrap();
                fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
        fn check(&self) -> std::io::Result<bool> {
            use std::os::unix::fs::MetadataExt;
            let meta = std::fs::metadata(&self.0).unwrap();
            published_components(&self.0, meta.uid(), meta.gid())
        }
        fn first(&self) -> std::path::PathBuf { self.0.join(BINARIES[0].trim_start_matches('/')) }
    }
    #[cfg(unix)]
    impl Drop for Fixture { fn drop(&mut self) { std::fs::remove_dir_all(&self.0).unwrap(); } }

    #[cfg(unix)]
    #[test]
    fn service_protected_layout_fake_root_requires_complete_publication() {
        let tree = Fixture::new();
        assert!(!tree.check().unwrap());
        tree.populate();
        assert!(tree.check().unwrap());
        assert!(tree.check().unwrap()); // re-install is read-only/idempotent
        std::fs::remove_file(tree.first()).unwrap();
        assert!(tree.check().is_err());
    }

    #[cfg(unix)]
    #[test]
    fn service_protected_layout_fake_root_refuses_links_modes_and_owner() {
        use std::{fs, os::unix::fs::{symlink, PermissionsExt}};
        let tree = Fixture::new(); tree.populate();
        fs::set_permissions(tree.first(), fs::Permissions::from_mode(0o775)).unwrap();
        assert!(tree.check().is_err());
        fs::set_permissions(tree.first(), fs::Permissions::from_mode(0o644)).unwrap();
        assert!(tree.check().is_err());
        #[cfg(target_os = "linux")]
        {
            fs::set_permissions(tree.first(), fs::Permissions::from_mode(0o4755)).unwrap();
            assert!(tree.check().is_err());
        }
        fs::set_permissions(tree.first(), fs::Permissions::from_mode(0o755)).unwrap();
        assert!(published_components(&tree.0, u32::MAX, u32::MAX).is_err());
        let alias = tree.0.join("alias"); fs::hard_link(tree.first(), &alias).unwrap();
        assert!(tree.check().is_err()); fs::remove_file(&alias).unwrap();
        fs::rename(tree.first(), &alias).unwrap(); symlink(&alias, tree.first()).unwrap();
        assert!(tree.check().is_err());
    }

    #[cfg(unix)]
    #[test]
    fn service_protected_layout_fake_root_refuses_writable_or_linked_ancestors() {
        use std::{fs, os::unix::fs::{symlink, PermissionsExt}};
        let tree = Fixture::new(); tree.populate();
        let parent = tree.first().parent().unwrap().to_path_buf();
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o777)).unwrap();
        assert!(tree.check().is_err());
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o755)).unwrap();
        let moved = tree.0.join("moved"); fs::rename(&parent, &moved).unwrap(); symlink(&moved, parent).unwrap();
        assert!(tree.check().is_err());
    }

    #[cfg(unix)]
    #[test]
    fn service_protected_layout_migrated_home_keeps_root_parent_and_private_child() {
        use std::{fs, os::unix::fs::{MetadataExt, PermissionsExt, symlink}};
        let tree = Fixture::new();
        let metadata = fs::metadata(&tree.0).unwrap();
        let child = tree.0.join(".duduclaw");
        fs::create_dir(&child).unwrap();
        fs::set_permissions(&tree.0, fs::Permissions::from_mode(0o711)).unwrap();
        fs::set_permissions(&child, fs::Permissions::from_mode(0o700)).unwrap();
        let check = || migrated_home_is_private(&tree.0, metadata.uid(), metadata.gid(),
            metadata.uid(), metadata.gid()).unwrap();
        assert!(check());
        assert!(!migrated_home_is_private(&tree.0, u32::MAX, metadata.gid(), metadata.uid(), metadata.gid()).unwrap());
        assert!(!migrated_home_is_private(&tree.0, metadata.uid(), metadata.gid(), u32::MAX, metadata.gid()).unwrap());
        fs::set_permissions(&child, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(!check());
        fs::set_permissions(&child, fs::Permissions::from_mode(0o700)).unwrap();
        fs::set_permissions(&tree.0, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(!check());
        fs::set_permissions(&tree.0, fs::Permissions::from_mode(0o711)).unwrap();
        fs::remove_dir(&child).unwrap();
        symlink("/tmp", &child).unwrap();
        assert!(!check());
    }

    #[cfg(unix)]
    #[test]
    fn service_protected_layout_parse_roots_reuse_only_empty_mountpoint_skeleton() {
        use std::{fs, os::unix::fs::{MetadataExt, symlink}};
        let tree = Fixture::new();
        let meta = fs::metadata(&tree.0).unwrap();
        let check = || parse_root_is_reusable(&tree.0, meta.uid(), meta.gid()).unwrap();
        assert!(check());
        for dir in [".tmp", "dev", "etc", "usr", "var/tmp"] { fs::create_dir_all(tree.0.join(dir)).unwrap(); }
        assert!(check());
        fs::write(tree.0.join("etc/shadow"), b"not allowed").unwrap();
        assert!(!check()); fs::remove_file(tree.0.join("etc/shadow")).unwrap();
        fs::remove_dir(tree.0.join("usr")).unwrap();
        symlink("/usr", tree.0.join("usr")).unwrap();
        assert!(!check());
    }

}
