//! Installer evidence and removal policy; std-only for the standalone harness.
use std::{collections::BTreeSet, path::Path};

pub(super) const EXTRA_UNITS: &[&str] = &[
    "duduclaw-gateway.service", "duduclaw-auto-upgrade.timer", "duduclaw-auto-upgrade.service",
    "duduclaw-migrate-import.socket", "duduclaw-migrate-import.service",
];

/// R8z-8: spawnd's transient units, by their exact shapes
/// (`duduclaw_spawnd::systemd::run_unit`, `employee_slice`): `duduclaw-run-` +
/// 32 lowercase hex + `.service`, `duduclaw-employees-` + 32 lowercase hex +
/// `.slice`. PID 1 loads them from `/run/systemd/transient`, which precedes
/// `/etc/systemd/system`, so an `/etc` mask never applies to them: removal
/// stops them and proves them inactive, it never masks them.
pub(super) fn runtime_unit(name: &str) -> bool {
    for (prefix, suffix) in [("duduclaw-run-", ".service"), ("duduclaw-employees-", ".slice")] {
        if let Some(id) = name.strip_prefix(prefix).and_then(|s| s.strip_suffix(suffix)) {
            return id.len() == 32 && id.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
        }
    }
    false
}

/// R8z-8: a template unit file (no loaded unit object; its persistent mask
/// fences every instance).
pub(super) fn template_unit(name: &str) -> bool { name.ends_with("@.service") || name.ends_with("@.timer") }

/// R8z-8: removal stop order — activation sources (timers, sockets), the
/// gateway and the updater, spawnd's employee runs, the other services, the
/// per-employee slices, the employees and TCB slices, then `duduclaw.slice`.
pub(super) fn stop_rank(name: &str) -> u8 {
    if name.ends_with(".timer") || name.ends_with(".socket") { 0 }
    else if name == "duduclaw-gateway.service" || name == "duduclaw-updated.service" { 1 }
    else if name.ends_with(".service") && runtime_unit(name) { 2 }
    else if name.ends_with(".service") { 3 }
    else if runtime_unit(name) { 4 }
    else if name != "duduclaw.slice" { 5 }
    else { 6 }
}

/// `units` in [`stop_rank`] order (by name within a rank).
pub(super) fn stop_order(units: &[String]) -> Vec<String> {
    let mut ordered = units.to_vec();
    ordered.sort_by(|a, b| (stop_rank(a), a).cmp(&(stop_rank(b), b)));
    ordered
}

pub(super) fn owned_unit(name: &str) -> bool {
    if super::layout::UNITS.iter().any(|(unit, _)| *unit == name) || EXTRA_UNITS.contains(&name) { return true; }
    if runtime_unit(name) { return true; }
    // `duduclaw-isolation-probe@` is `duduclaw_isolation::ids::ISOLATION_PROBE_UNIT_PREFIX`
    // (R7c; a literal: this module stays std-only; equality is tested in protected_linux_tests.rs).
    for prefix in ["duduclaw-parse@", "duduclaw-import@", "duduclaw-migrate-expire@", "duduclaw-isolation-probe@"] {
        if let Some(instance) = name.strip_prefix(prefix).and_then(|s| s.strip_suffix(".service").or_else(|| s.strip_suffix(".timer"))) {
            return !instance.is_empty() && instance.len() <= 128
                && instance.bytes().all(|b| b.is_ascii_alphanumeric() || b"_-.".contains(&b));
        }
    }
    false
}

/// R8z-8: a listed name for a message — escaped, at most 128 characters.
pub(super) fn shown_name(name: &str) -> String {
    name.chars().take(128).flat_map(char::escape_default).collect()
}

/// Every DuDuClaw unit PID 1 lists. A `duduclaw` name of no owned shape
/// refuses by name (R8z-8): it is never stopped, masked or removed.
pub(super) fn inventory(text: &str) -> Result<BTreeSet<String>, String> {
    let mut result = BTreeSet::new();
    for line in text.lines() {
        let Some(name) = line.split_whitespace().next() else { continue; };
        if name == "duduclaw.service" || name.strip_prefix("duduclaw-").is_some() || name == "duduclaw.slice" {
            if !owned_unit(name) {
                return Err(format!("發現名稱不符合本產品單元格式的 DuDuClaw 單元 {}；不會停止、封鎖或移除它，也未移除執行檔。請先確認它的來源並處理後，再執行同一個移除命令。", shown_name(name)));
            }
            result.insert(name.to_owned());
        }
        if result.len() > 10_000 { return Err("unit inventory exceeds limit".into()); }
    }
    Ok(result)
}

fn mount_path(value: &str) -> Result<String, &'static str> {
    let mut bytes = Vec::new();
    let mut input = value.bytes();
    while let Some(byte) = input.next() {
        if byte == b'\\' {
            let escape = [input.next(), input.next(), input.next()];
            bytes.push(match escape {
                [Some(b'0'), Some(b'4'), Some(b'0')] => b' ',
                [Some(b'0'), Some(b'1'), Some(b'1')] => b'\t',
                [Some(b'0'), Some(b'1'), Some(b'2')] => b'\n',
                [Some(b'1'), Some(b'3'), Some(b'4')] => b'\\',
                _ => return Err("invalid mountinfo escape"),
            });
        } else { bytes.push(byte); }
    }
    String::from_utf8(bytes).map_err(|_| "invalid mountinfo path")
}

// Retained data may be a separate host filesystem. Only installation/employee
// mounts block removal; no lazy/forced unmount and no deletion of image files.
pub(super) fn mounts_clear(text: &str) -> Result<bool, &'static str> {
    let roots = ["/usr/lib/duduclaw", "/usr/libexec/duduclaw", "/opt/duduclaw-isolated",
        "/opt/duduclaw", "/usr/local/libexec/duduclaw", "/usr/libexec/duduclaw-updated-launch",
        "/usr/local/sbin/duduclaw-auto-upgrade", "/etc/systemd/system",
        "/var/lib/duduclaw-spawnd", "/run/duduclaw-spawnd", "/var/lib/duduclaw/agents"];
    if text.is_empty() { return Err("mountinfo is empty"); }
    for line in text.lines() {
        let fields: Vec<_> = line.split_whitespace().collect();
        let separator = fields.iter().position(|s| *s == "-").ok_or("invalid mountinfo")?;
        if separator < 6 || fields.len() < separator + 4 { return Err("invalid mountinfo"); }
        for value in [fields[3], fields[4], fields[separator + 2]] {
            let decoded = mount_path(value)?;
            if roots.iter().any(|root| Path::new(&decoded).starts_with(root)) { return Ok(false); }
        }
    }
    Ok(true)
}

pub(super) fn cgroup_empty(events: &str) -> Result<bool, &'static str> {
    let values: Vec<_> = events.lines().filter_map(|line| {
        let mut fields = line.split_whitespace();
        (fields.next() == Some("populated")).then(|| (fields.next(), fields.next()))
    }).collect();
    match values.as_slice() {
        [(Some("0"), None)] => Ok(true),
        [(Some("1"), None)] => Ok(false),
        _ => Err("invalid cgroup population readback"),
    }
}

// Root and ownership are fixed to / and 0:0 by the installer. The explicit
// anchor lets tests exercise the identical filesystem transaction in a fake root.
#[cfg(unix)]
pub(super) fn fence_unit_files(root: &Path, uid: u32, gid: u32, unit: &str) -> std::io::Result<()> {
    use std::{fs, io::{Read, Write}, os::unix::fs::{MetadataExt, OpenOptionsExt, symlink}};
    let invalid = || std::io::Error::other("unsafe uninstall unit or recovery file");
    if !owned_unit(unit) { return Err(invalid()); }
    let units = root.join("etc/systemd/system");
    let backup = root.join("var/lib/duduclaw-protected/uninstall-units");
    for base in [&units, &backup] {
        for path in base.ancestors() {
            let meta = fs::symlink_metadata(path)?;
            if !meta.is_dir() || meta.file_type().is_symlink() || meta.uid() != uid || meta.gid() != gid
                || meta.mode() & 0o022 != 0 { return Err(invalid()); }
            if path == root { break; }
        }
    }
    if fs::symlink_metadata(&backup)?.mode() & 0o7777 != 0o700 { return Err(invalid()); }
    let regular = |path: &Path| -> std::io::Result<Vec<u8>> {
        let meta = fs::symlink_metadata(path)?;
        if !meta.is_file() || meta.file_type().is_symlink() || meta.uid() != uid || meta.gid() != gid
            || meta.nlink() != 1 || meta.mode() & 0o022 != 0 || meta.len() > 1024 * 1024 { return Err(invalid()); }
        let mut bytes = Vec::new(); fs::File::open(path)?.take(1024 * 1024 + 1).read_to_end(&mut bytes)?;
        if bytes.len() > 1024 * 1024 { return Err(invalid()); }
        Ok(bytes)
    };
    let path = units.join(unit);
    match fs::symlink_metadata(&path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            if meta.uid() != uid || meta.gid() != gid || fs::read_link(&path)? != Path::new("/dev/null") { return Err(invalid()); }
        }
        Ok(_) => {
            let bytes = regular(&path)?;
            let saved = backup.join(unit);
            match fs::symlink_metadata(&saved) {
                Ok(_) => if regular(&saved)? != bytes { return Err(invalid()); },
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    let next = backup.join(format!(".{unit}.next"));
                    match fs::symlink_metadata(&next) {
                        Ok(_) => { regular(&next)?; fs::remove_file(&next)?; }
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
                        Err(error) => return Err(error),
                    }
                    let mut file = fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&next)?;
                    file.write_all(&bytes)?; file.sync_all()?;
                    fs::rename(&next, &saved)?; fs::File::open(&backup)?.sync_all()?;
                }
                Err(error) => return Err(error),
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
        Err(error) => return Err(error),
    }
    let next = units.join(format!(".uninstall-mask-{unit}"));
    match fs::symlink_metadata(&next) {
        Ok(meta) if meta.file_type().is_symlink() && meta.uid() == uid && meta.gid() == gid
            && fs::read_link(&next)? == Path::new("/dev/null") => fs::remove_file(&next)?,
        Ok(_) => return Err(invalid()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
        Err(error) => return Err(error),
    }
    symlink("/dev/null", &next)?; fs::rename(&next, &path)?;
    fs::File::open(&units)?.sync_all()?;
    Ok(())
}

pub(super) trait RemovalHost {
    fn fence(&mut self, units: &[String]) -> Result<(), String>;
    fn stop(&mut self, units: &[String]) -> Result<(), String>;
    fn quiescent(&mut self, units: &[String]) -> Result<(), String>;
    fn remove(&mut self) -> Result<(), String>;
}

// Fence BEFORE stopping (nothing a socket or timer activates can come back in
// between). R8z-8: the fence only writes the masks; EVERY unit is then asked to
// stop — also when fencing failed part-way — and only then is anything proven
// (masked where a mask applies, inactive, no new unit, no process, no mount).
// So a refusal after the first mask always comes after every stop: the host is
// never left with masked units still running. Every retry repeats live
// evidence; a previous attempt cannot act as a receipt proving that processes
// or mounts are still absent.
pub(super) fn remove_installation(host: &mut impl RemovalHost, units: &[String]) -> Result<(), String> {
    if units.iter().any(|unit| !owned_unit(unit)) { return Err("unknown removal unit".into()); }
    let fenced = host.fence(units);
    let stopped = host.stop(units);
    fenced?;
    stopped?;
    host.quiescent(units)?;
    host.remove()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn service_protected_lifecycle_units_are_exact_and_unknown_activation_refuses() {
        assert!(owned_unit("duduclaw-run-0123456789abcdef0123456789abcdef.service"));
        assert!(owned_unit("duduclaw-parse@3-1.service"));
        // R7a: a loaded self-test unit is inventoried (fenced, stopped, removed), never refused as unknown.
        assert!(owned_unit("duduclaw-isolation-selftest.service"));
        assert_eq!(inventory("duduclaw-isolation-selftest.service static\n").unwrap().len(), 1);
        // R7c: the probe template file and its per-account instances are owned too.
        assert!(owned_unit("duduclaw-isolation-probe@.service"));
        assert!(owned_unit("duduclaw-isolation-probe@duduclaw-emp-70000.service"));
        assert_eq!(inventory("duduclaw-isolation-probe@.service static\n\
            duduclaw-isolation-probe@duduclaw-emp-70000.service static\n").unwrap().len(), 2);
        assert!(!owned_unit("duduclaw-isolation-probe@../x.service"));
        for name in ["--help", "duduclaw-run-x.service", "duduclaw-parse@../x.service",
            "duduclaw-gateway.service.evil", "duduclaw-other.service"] { assert!(!owned_unit(name)); }
        assert!(inventory("duduclaw-other.service enabled\n").is_err());
        assert_eq!(inventory("sshd.service enabled\nduduclaw-gateway.service enabled\n").unwrap().len(), 1);
    }
    /// R8z-8: spawnd's transient names by their exact shapes only; the stop
    /// order is runs, then per-employee slices, then the parent slices.
    #[test]
    fn service_protected_lifecycle_runtime_units_are_exact_and_stop_inside_out() {
        let slice = "duduclaw-employees-0123456789abcdef0123456789abcdef.slice";
        let run = "duduclaw-run-fedcba9876543210fedcba9876543210.service";
        assert!(runtime_unit(slice) && runtime_unit(run) && owned_unit(slice) && owned_unit(run));
        for name in ["duduclaw-employees.slice", "duduclaw-employees-evil.slice", "duduclaw-run-../../x.service",
            "duduclaw-employees-0123456789ABCDEF0123456789abcdef.slice", "duduclaw-employees-0123456789abcdef0123456789abcdef.service",
            "duduclaw-run-0123456789abcdef0123456789abcdef0.service", "duduclaw-foo.service", "duduclaw-gateway.service"] {
            assert!(!runtime_unit(name), "{name}");
        }
        for name in ["duduclaw-employees-evil.slice", "duduclaw-run-../../x.service", "duduclaw-foo.service"] {
            assert!(!owned_unit(name), "{name}");
            let error = inventory(&format!("{name} loaded active running x\n")).unwrap_err();
            assert!(error.contains(name), "{error}");
        }
        let units: Vec<String> = ["duduclaw.slice", "duduclaw-tcb.slice", slice, "duduclaw-employees.slice",
            "duduclaw-spawnd.service", run, "duduclaw-gateway.service", "duduclaw-spawnd.socket", "duduclaw-auto-upgrade.timer"]
            .into_iter().map(String::from).collect();
        assert_eq!(stop_order(&units), ["duduclaw-auto-upgrade.timer", "duduclaw-spawnd.socket", "duduclaw-gateway.service",
            run, "duduclaw-spawnd.service", slice, "duduclaw-employees.slice", "duduclaw-tcb.slice", "duduclaw.slice"]);
    }
    #[test]
    fn service_protected_lifecycle_mounts_are_component_matched_and_decoded() {
        let root = "1 0 8:1 / / rw - ext4 /dev/sda rw\n";
        assert_eq!(mounts_clear(root), Ok(true));
        assert!(mounts_clear("").is_err());
        assert!(mounts_clear("malformed").is_err());
        for mount in ["/usr/lib/duduclaw/parse-root", "/run/duduclaw-spawnd/run", "/var/lib/duduclaw/agents/a/home"] {
            assert_eq!(mounts_clear(&format!("{root}2 1 0:1 / {mount} rw - tmpfs tmpfs rw\n")), Ok(false));
        }
        assert_eq!(mounts_clear(&format!("{root}2 1 8:2 / /var/lib/duduclaw rw - ext4 /dev/sdb rw\n")), Ok(true));
        assert_eq!(mounts_clear(&format!("{root}2 1 8:2 / /usr/lib/duduclaw-other rw - ext4 /dev/sdb rw\n")), Ok(true));
        assert_eq!(mount_path("/a\\040b"), Ok("/a b".into()));
        assert!(mount_path("/a\\000b").is_err());
    }
    #[test]
    fn service_protected_lifecycle_cgroup_requires_exact_population() {
        assert_eq!(cgroup_empty("populated 0\nfrozen 0\n"), Ok(true));
        assert_eq!(cgroup_empty("populated 1\n"), Ok(false));
        for value in ["", "populated 0\npopulated 0", "populated 0 extra", "populated false"] {
            assert!(cgroup_empty(value).is_err());
        }
    }
    struct Fake { fail: Option<&'static str>, calls: Vec<&'static str> }
    impl Fake { fn call(&mut self, op: &'static str) -> Result<(), String> {
        self.calls.push(op); if self.fail == Some(op) { Err(op.into()) } else { Ok(()) }
    } }
    impl RemovalHost for Fake {
        fn fence(&mut self, _: &[String]) -> Result<(), String> { self.call("fence") }
        fn stop(&mut self, _: &[String]) -> Result<(), String> { self.call("stop") }
        fn quiescent(&mut self, _: &[String]) -> Result<(), String> { self.call("quiescent") }
        fn remove(&mut self) -> Result<(), String> { self.call("remove") }
    }
    #[test]
    fn service_protected_lifecycle_never_removes_before_live_quiescence() {
        let units = vec!["duduclaw-gateway.service".into()];
        for failed in ["fence", "stop", "quiescent"] {
            let mut host = Fake { fail: Some(failed), calls: vec![] };
            assert!(remove_installation(&mut host, &units).is_err());
            assert!(!host.calls.contains(&"remove"));
            // R8z-8: a failed fence still stops every unit (no masked unit left running).
            assert!(host.calls.contains(&"stop"), "{failed}: {:?}", host.calls);
        }
        let mut host = Fake { fail: None, calls: vec![] };
        let foreign = vec!["duduclaw-gateway.service".into(), "duduclaw-employees-evil.slice".into()];
        assert!(remove_installation(&mut host, &foreign).is_err());
        assert!(host.calls.is_empty(), "an unknown name is never fenced or stopped: {:?}", host.calls);
        let mut host = Fake { fail: None, calls: vec![] };
        remove_installation(&mut host, &units).unwrap();
        assert_eq!(host.calls, ["fence", "stop", "quiescent", "remove"]);
        host.calls.clear();
        remove_installation(&mut host, &units).unwrap();
        assert_eq!(host.calls, ["fence", "stop", "quiescent", "remove"]);
    }

    #[cfg(unix)]
    #[test]
    fn service_protected_lifecycle_fake_root_mask_backup_and_retry() {
        use std::{fs, os::unix::fs::{MetadataExt, PermissionsExt, symlink}};
        let root = std::env::temp_dir().join(format!("iso-i2-fence-{}", std::process::id()));
        fs::create_dir(&root).unwrap(); fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let owner = fs::metadata(&root).unwrap();
        let units = root.join("etc/systemd/system"); fs::create_dir_all(&units).unwrap();
        let backup = root.join("var/lib/duduclaw-protected/uninstall-units"); fs::create_dir_all(&backup).unwrap();
        fs::set_permissions(&backup, fs::Permissions::from_mode(0o700)).unwrap();
        let name = "duduclaw-gateway.service";
        let path = units.join(name);
        fs::write(&path, b"original signed unit\n").unwrap();
        let key = root.join("var/lib/duduclaw-protected/root.key"); fs::write(&key, b"retain secret").unwrap();
        let fence = || fence_unit_files(&root, owner.uid(), owner.gid(), name);
        fence().unwrap(); fence().unwrap();
        assert_eq!(fs::read_link(&path).unwrap(), Path::new("/dev/null"));
        assert_eq!(fs::read(backup.join(name)).unwrap(), b"original signed unit\n");
        assert_eq!(fs::read(&key).unwrap(), b"retain secret");
        fs::remove_file(&path).unwrap(); fs::write(&path, b"changed after interrupted uninstall").unwrap();
        assert!(fence().is_err()); // old evidence must never replace a changed unit
        assert_eq!(fs::read(&path).unwrap(), b"changed after interrupted uninstall");
        fs::remove_file(&path).unwrap(); symlink(&key, &path).unwrap();
        assert!(fence().is_err()); assert_eq!(fs::read(&key).unwrap(), b"retain secret");
        fs::remove_file(&path).unwrap();
        fs::remove_file(backup.join(name)).unwrap();
        symlink(&key, backup.join(name)).unwrap(); fs::write(&path, b"unit").unwrap();
        assert!(fence().is_err()); // no following replaced recovery files
        assert_eq!(fs::read(&key).unwrap(), b"retain secret");
        fs::remove_dir_all(root).unwrap();
    }
}
