//! Coordinated removal. Keep identities, trust, data and recovery generations.
use super::*;
use std::{collections::BTreeSet, io::Read, path::PathBuf};

fn read_bounded(path: &Path, limit: u64) -> Result<String> {
    let mut file = fs::File::open(path)?;
    let mut text = String::new();
    Read::by_ref(&mut file).take(limit + 1).read_to_string(&mut text)?;
    if text.len() as u64 > limit { return Err(refused("移除檢查資料超過上限。")); }
    Ok(text)
}

fn unit_inventory() -> Result<BTreeSet<String>> {
    let mut units = BTreeSet::new();
    for args in [vec!["list-unit-files", "--no-legend", "--no-pager", "--type=service,socket,timer,slice"],
        vec!["list-units", "--all", "--plain", "--no-legend", "--no-pager", "--type=service,socket,timer,slice"]] {
        let bytes = checked("systemctl", &args)?.stdout;
        if bytes.len() > 4 * 1024 * 1024 { return Err(refused("服務清單超過上限。")); }
        let text = String::from_utf8(bytes).map_err(|_| refused("無法讀取服務清單。"))?;
        units.extend(lifecycle::inventory(&text).map_err(refused)?);
    }
    Ok(units)
}

fn removal_files() -> Vec<PathBuf> {
    // Persistent floors, keys, identity tombstones, update journal and generation
    // receipts deliberately survive. They are needed for recovery and anti-rollback.
    let mut files: Vec<_> = layout::BINARIES.iter().copied().chain([BINARY, LAUNCHER,
        "/usr/local/sbin/duduclaw-auto-upgrade"]).map(PathBuf::from).collect();
    for name in layout::UNITS.iter().map(|(name, _)| *name).chain(lifecycle::EXTRA_UNITS.iter().copied()) {
        files.push(Path::new("/etc/systemd/system").join(name));
    }
    files
}

fn validate_removals(files: &[PathBuf]) -> Result<()> {
    for path in files {
        // Check every existing ancestor, including when the final name is absent.
        // A failed partial removal cannot turn a symlink into a new destination.
        for parent in path.parent().ok_or_else(|| refused("無效的移除路徑。"))?.ancestors() {
            match fs::symlink_metadata(parent) {
                Ok(_) => trusted(parent, true)?,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
                Err(error) => return Err(error.into()),
            }
        }
        match fs::symlink_metadata(path) {
            Ok(meta) => {
                if persistent_mask(path)? { continue; }
                trusted(path, false)?;
                if meta.nlink() != 1 { return Err(refused("移除目標含有硬連結。")); }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn persistent_mask(path: &Path) -> Result<bool> {
    if path.parent() != Some(Path::new("/etc/systemd/system"))
        || !path.file_name().and_then(|s| s.to_str()).is_some_and(lifecycle::owned_unit) { return Ok(false); }
    let meta = fs::symlink_metadata(path)?;
    if !meta.file_type().is_symlink() { return Ok(false); }
    if meta.uid() != 0 || meta.gid() != 0 || fs::read_link(path)? != Path::new("/dev/null") {
        return Err(refused("非預期的服務封鎖連結。"));
    }
    Ok(true)
}

fn mask_unit(unit: &str) -> Result<()> {
    mkdir("/var/lib/duduclaw-protected/uninstall-units", 0o700)?;
    lifecycle::fence_unit_files(Path::new("/"), 0, 0, unit)?;
    Ok(())
}

fn service_uids() -> Result<BTreeSet<u32>> {
    let mut uids = BTreeSet::new();
    for name in std::iter::once("duduclaw").chain(layout::ACCOUNTS.iter().copied()) {
        let output = command("getent", &["passwd", name])?;
        if output.status.code() == Some(2) { continue; }
        if !output.status.success() { return Err(refused("無法確認服務身分。")); }
        let text = String::from_utf8(output.stdout).map_err(|_| refused("無效的帳號資料。"))?;
        let parts: Vec<_> = text.trim_end_matches('\n').split(':').collect();
        if parts.len() != 7 || parts[0] != name { return Err(refused("無效的帳號資料。")); }
        let uid = parts[2].parse::<u32>().map_err(|_| refused("無效的帳號身分。"))?;
        if uid == 0 { return Err(refused("服務帳號不能是 root。")); }
        uids.insert(uid);
    }
    Ok(uids)
}

fn processes_clear() -> Result<()> {
    let uids = service_uids()?;
    let mut count = 0;
    for entry in fs::read_dir("/proc")? {
        let entry = entry?;
        let Some(pid) = entry.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else { continue; };
        if pid == std::process::id() { continue; }
        count += 1;
        if count > 100_000 { return Err(refused("程序檢查超過上限。")); }
        let status = match read_bounded(&entry.path().join("status"), 64 * 1024) {
            Ok(value) => value,
            Err(error) if entry.path().try_exists().is_ok_and(|exists| !exists) => { let _ = error; continue; }
            Err(error) => return Err(error),
        };
        let uid_line = status.lines().find_map(|line| line.strip_prefix("Uid:"))
            .ok_or_else(|| refused("無法確認程序身分。"))?;
        let ids: Vec<_> = uid_line.split_whitespace().map(str::parse::<u32>).collect::<std::result::Result<_, _>>()
            .map_err(|_| refused("無效的程序身分。"))?;
        if ids.len() != 4 { return Err(refused("無效的程序身分。")); }
        if ids.iter().any(|uid| uids.contains(uid) || (70000..=99999).contains(uid)) {
            return Err(refused("仍有服務或員工程序，未移除執行檔。"));
        }
        match fs::read_link(entry.path().join("exe")) {
            Ok(path) => {
                if ["/opt/duduclaw-isolated", "/opt/duduclaw", "/usr/lib/duduclaw", "/usr/libexec/duduclaw",
                    "/usr/libexec/duduclaw-updated-launch", "/usr/local/libexec/duduclaw"]
                    .iter().any(|root| path.starts_with(root)) {
                    return Err(refused("仍有隔離元件正在執行，未移除執行檔。"));
                }
            }
            // Kernel threads and zombies have no executable. Their UIDs were
            // checked above; any unreadable live executable is a refusal.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

struct NativeRemoval { files: Vec<PathBuf> }
impl lifecycle::RemovalHost for NativeRemoval {
    fn fence(&mut self, units: &[String]) -> std::result::Result<(), String> {
        for unit in units {
            mask_unit(unit).map_err(|e| e.to_string())?;
        }
        checked("systemctl", &["daemon-reload"]).map_err(|e| e.to_string())?;
        for unit in units {
            // Templates have no loaded unit object. Their persistent /etc mask
            // fences all future instances; live instances are checked below.
            if unit.ends_with("@.service") || unit.ends_with("@.timer") {
                if !persistent_mask(&Path::new("/etc/systemd/system").join(unit)).map_err(|e| e.to_string())? {
                    return Err("服務範本未封鎖。".into());
                }
                continue;
            }
            let result = checked("systemctl", &["show", unit, "--property=LoadState", "--value"])
                .map_err(|e| e.to_string())?;
            if String::from_utf8_lossy(&result.stdout).trim() != "masked" {
                return Err("無法證明服務啟動已封鎖；未移除執行檔。".into());
            }
        }
        Ok(())
    }
    fn stop(&mut self, units: &[String]) -> std::result::Result<(), String> {
        // Timers/sockets first; gateway and updater before spawnd and workers.
        let mut ordered = units.to_vec();
        ordered.sort_by_key(|name| if name.ends_with(".timer") || name.ends_with(".socket") { 0 }
            else if name == UNIT || name == "duduclaw-updated.service" { 1 }
            else if name.ends_with(".slice") { 3 } else { 2 });
        for unit in &ordered {
            // Templates do not denote running units; their instances were also
            // captured from list-units before masks were installed.
            if unit.ends_with("@.service") || unit.ends_with("@.timer") { continue; }
            checked("systemctl", &["stop", unit]).map_err(|e| e.to_string())?;
        }
        Ok(())
    }
    fn quiescent(&mut self, units: &[String]) -> std::result::Result<(), String> {
        let verify = || -> Result<()> {
            if !unit_inventory()?.iter().all(|unit| units.contains(unit)) {
                return Err(refused("移除期間出現新的服務；保留封鎖並停止移除。"));
            }
            for unit in units {
                if unit.ends_with("@.service") || unit.ends_with("@.timer") { continue; }
                let output = checked("systemctl", &["show", unit, "--property=ActiveState,LoadState"])?;
                let text = String::from_utf8(output.stdout).map_err(|_| refused("無效的服務狀態。"))?;
                if !text.lines().any(|line| line == "LoadState=masked")
                    || !text.lines().any(|line| matches!(line, "ActiveState=inactive" | "ActiveState=failed")) {
                    return Err(refused("服務尚未停止。"));
                }
            }
            for root in ["/sys/fs/cgroup/duduclaw.slice", "/sys/fs/cgroup/system.slice/duduclaw-gateway.service"] {
                let path = Path::new(root).join("cgroup.events");
                match fs::symlink_metadata(&path) {
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
                    Err(error) => return Err(error.into()),
                    Ok(_) => if !lifecycle::cgroup_empty(&read_bounded(&path, 4096)?).map_err(refused)? {
                        return Err(refused("服務控制群組仍有程序。"));
                    },
                }
            }
            processes_clear()?;
            if !lifecycle::mounts_clear(&read_bounded(Path::new("/proc/self/mountinfo"), 16 * 1024 * 1024)?).map_err(refused)? {
                return Err(refused("仍有隔離掛載；請先透過儲存服務清理，未強制卸載或刪除資料。"));
            }
            Ok(())
        };
        verify().map_err(|e| e.to_string())
    }
    fn remove(&mut self) -> std::result::Result<(), String> {
        let remove = || -> Result<()> {
            validate_removals(&self.files)?;
            for file in &self.files {
                // Keep persistent masks so a vendor unit or still-present
                // enablement link cannot resurrect a retained recovery binary.
                if fs::symlink_metadata(file).is_ok() && persistent_mask(file)? { continue; }
                match fs::remove_file(file) {
                    Ok(()) => fs::File::open(file.parent().unwrap())?.sync_all()?,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
                    Err(error) => return Err(error.into()),
                }
            }
            checked("systemctl", &["daemon-reload"])?;
            Ok(())
        };
        remove().map_err(|e| e.to_string())
    }
}

pub(super) fn run() -> Result<()> {
    let files = removal_files();
    validate_removals(&files)?;
    let units: Vec<_> = unit_inventory()?.into_iter().collect();
    mkdir("/var/lib/duduclaw-protected/uninstall-units", 0o700)?;
    let mut host = NativeRemoval { files };
    lifecycle::remove_installation(&mut host, &units).map_err(refused)?;
    println!("已停止並移除隔離服務與固定啟動器，服務保持封鎖。資料、員工身分紀錄、金鑰、更新信任、單元備份與復原世代皆保留；未刪除員工磁碟。");
    Ok(())
}

#[cfg(test)]
mod root_tests {
    use super::*;
    #[test]
    #[ignore = "requires a disposable native Linux VM with the complete trusted isolation fixture; removes its services"]
    fn iso_root_install_uninstall_preserves_recovery_and_is_repeatable() {
        assert_eq!(std::env::var("DUDUCLAW_ISO_DISPOSABLE").as_deref(), Ok("1"));
        gate().unwrap();
        assert!(layout::published_components(Path::new("/"), 0, 0).unwrap());
        let key = Path::new("/var/lib/duduclaw-protected/root.key");
        let original = fs::read(key).unwrap();
        run().unwrap();
        run().unwrap();
        // Do not print root-key bytes in an assertion failure.
        assert!(fs::read(key).unwrap() == original);
        for binary in layout::BINARIES { assert!(!Path::new(binary).exists()); }
        assert!(Path::new("/var/lib/duduclaw-protected/uninstall-units").is_dir());
    }
}
