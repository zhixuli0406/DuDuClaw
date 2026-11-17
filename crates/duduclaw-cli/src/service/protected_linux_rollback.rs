//! Rollback of an uncommitted v1.83 isolation bootstrap (plan §4.3).
//!
//! Window: any time before S9 Commit of a fresh install (`committed:false`).
//! After the commit only `service uninstall --protected-linux` exists. The
//! upgrade window (until `KeysRotated`, Q8) and its un-quarantine belong to R5
//! and are refused here.
//!
//! What it removes is exactly what a fresh bootstrap run can have created —
//! S1 proved every one of these paths absent before the journal existed:
//! the isolation and gateway units (stopped and disabled first), the fixed-path
//! programs, the gateway unit/launcher/recovery bound, the nightly requester,
//! `/usr/lib/duduclaw`, `/usr/libexec/duduclaw`, `/opt/duduclaw-isolated`,
//! `/etc/duduclaw-updated`, `/var/lib/duduclaw-updated`, `/var/lib/duduclaw-spawnd`,
//! `/var/log/duduclaw-spawnd`, `/var/lib/duduclaw-migrate`, `/run/duduclaw-spawnd`,
//! `egress.json`, `dns-broker.json`, the version intent/floor, the root key the
//! gateway's launcher created, and the gateway's data except its license. It
//! keeps the system accounts, the empty gateway home (root 0711 with the private
//! `.duduclaw` and the license), the protected-mode marker (it only narrows),
//! the bootstrap high-water record (anti-rollback, never lowered) and spawnd's
//! SSH/cron lockdown entries (they only deny). Deletion never follows a link,
//! never crosses a mount and is bounded; there is no `rm -rf`. Finally no
//! isolation fact may remain; otherwise the remaining paths are listed and the
//! journal stays in the rollback state so the same command can finish later.
//!
//! LOCK ORDER (see `protected_linux_bootstrap.rs`): 0 (the caller) → 1 (R1's
//! stage lock) → 2 (`version_lock`) → 3 (updater state, before its removal).
use super::*;
use super::bootstrap::{self as boot, Journal, Phase, Step, Store};
use std::{ffi::{CStr, CString}, os::{fd::FromRawFd, unix::ffi::OsStrExt}};
use duduclaw_updated::linux::{bootstrap_verify, files::Directory};

/// Stop order: the gateway, the nightly requester, every socket (so nothing is
/// re-activated), then the services.
const STOP_ORDER: [&str; 12] = [UNIT, boot::NIGHTLY_TIMER, "duduclaw-auto-upgrade.service",
    "duduclaw-egress.socket", "duduclaw-dns-broker.socket", "duduclaw-parse.socket", "duduclaw-spawnd.socket",
    "duduclaw-updated.service", "duduclaw-egress.service", "duduclaw-dns-broker.service", "duduclaw-spawnd.service",
    "duduclaw-employees.slice"];
/// The units a bootstrap run enables (S6, S8); the others have no `[Install]`.
const ENABLED: [&str; 9] = [UNIT, boot::NIGHTLY_TIMER, "duduclaw-egress.socket", "duduclaw-dns-broker.socket",
    "duduclaw-parse.socket", "duduclaw-spawnd.socket", "duduclaw-spawnd.service", "duduclaw-egress.service",
    "duduclaw-updated.service"];
/// Instances never have their own unit file; `systemctl stop` with a pattern
/// (no shell involved) stops the loaded ones and succeeds when none match.
const INSTANCE_PATTERNS: [&str; 4] = ["duduclaw-parse@*.service", "duduclaw-import@*.service",
    "duduclaw-run-*.service", "duduclaw-migrate-expire@*.timer"];
/// Root-owned trees an install run creates (all isolation facts or below one).
const TREES: [&str; 9] = ["/opt/duduclaw-isolated", "/etc/duduclaw-updated", "/var/lib/duduclaw-updated",
    "/var/lib/duduclaw-spawnd", "/var/log/duduclaw-spawnd", "/usr/lib/duduclaw", "/usr/libexec/duduclaw",
    "/var/lib/duduclaw-migrate", "/run/duduclaw-spawnd"];
/// Directories the run created that may be shared with other software only
/// in principle; removed only when empty.
const EMPTY_DIRECTORIES: [&str; 3] = ["/usr/local/libexec/duduclaw", "/opt/duduclaw/bin", "/opt/duduclaw"];
const MAX_TREE_ENTRIES: u32 = 200_000;
const MAX_DIRECTORY_ENTRIES: usize = 100_000;

fn unit_path(name: &str) -> String { format!("/etc/systemd/system/{name}") }

fn removal_files() -> Vec<String> {
    let mut files: Vec<String> = [UNIT_PATH, RECOVERY_BOUND, LAUNCHER, BINARY, VERSION_PENDING, VERSION_FLOOR,
        boot::NIGHTLY_HELPER, duduclaw_egress::daemon::CONFIG_PATH, duduclaw_egress::daemon::BROKER_CONFIG_PATH,
        duduclaw_core::root_key::KEY_PATH].iter().map(|path| (*path).to_owned()).collect();
    files.extend(layout::BINARIES.iter().map(|path| (*path).to_owned()));
    files.extend(layout::UNITS.iter().map(|(name, _)| unit_path(name)));
    files.extend(boot::NIGHTLY_UNITS.iter().map(|(name, _)| unit_path(name)));
    files
}

/// `service bootstrap-rollback --protected-linux`.
pub(super) fn run_standalone() -> Result<()> {
    duduclaw_updated::linux::harden().map_err(|_| refused("無法以受限的 root 權限執行回退。"))?;
    duduclaw_updated::linux::require_root().map_err(|_| refused("無法以受限的 root 權限執行回退。"))?;
    let Some(store) = Store::open_existing()? else {
        return Err(refused("這台主機沒有進行中的隔離安裝，不需要回退。"));
    };
    let _run = store.run_lock()?;
    let journal = store.read()?;
    run(&store, journal, None)
}

/// The caller holds lock 0 (`Store::run_lock`).
pub(super) fn run(store: &Store, journal: Option<Journal>, generation: Option<&str>) -> Result<()> {
    let Some(journal) = journal else {
        return Err(refused("這台主機沒有進行中的隔離安裝，不需要回退。"));
    };
    boot::rollback_permitted(&journal, generation)?;
    let _stage = stage_lock()?;
    let _version = version_lock()?;
    let journal = if journal.step == Step::Rollback { journal } else {
        let marked = journal.at(Step::Rollback, Phase::Entered);
        store.write(Some(&journal), &marked)?;
        marked
    };
    println!("正在回退尚未完成的隔離安裝……");
    stop_units()?;
    removal::processes_clear()?;
    for path in removal_files() { remove_file_checked(&path)?; }
    let reload = command("systemctl", &["daemon-reload"])?;
    if !reload.status.success() { return Err(refused("systemd 指令失敗或回應無法判讀。")); }
    {
        // Lock 3 while the updater state is removed (its writers are stopped).
        let _state_lock = match Directory::open(Path::new(duduclaw_updated::linux::STATE_ROOT)) {
            Ok(state) => Some(state.lock().map_err(|_| refused("更新服務的狀態正在使用中，請稍後再回退。"))?),
            Err(duduclaw_updated::Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(_) => return Err(refused("更新服務的狀態目錄不是 root 擁有的安全目錄，已停止回退。")),
        };
        for tree in TREES { remove_tree(tree)?; }
    }
    for directory in EMPTY_DIRECTORIES { remove_if_empty(directory)?; }
    clear_gateway_data()?;
    let remaining: Vec<&str> = duduclaw_core::isolation_install::FACTS.iter().copied()
        .filter(|fact| fs::symlink_metadata(fact).map(|_| true).unwrap_or_else(|error| error.kind() != std::io::ErrorKind::NotFound))
        .collect();
    if !remaining.is_empty() || duduclaw_core::isolation_install::installed_under(Path::new("/")) {
        println!("[未通過] 以下隔離安裝的路徑仍然存在：");
        for path in &remaining { println!("  - {path}"); }
        return Err(refused("回退尚未完成；請檢查上列路徑後再執行同一個回退命令。"));
    }
    remove_stage(&journal.generation)?;
    store.remove()?;
    println!("已回退到安裝前的狀態。保留：系統帳號、空的 Gateway 資料目錄與團隊版授權、受保護模式標記，以及安裝包的防降版紀錄。");
    Ok(())
}

/// Lock 1 of the LOCK ORDER: R1's stage lock (never waits here).
fn stage_lock() -> Result<Option<fs::File>> {
    let protected = Directory::open(Path::new(bootstrap_verify::PROTECTED_ROOT))
        .map_err(|_| refused("無法開啟受保護狀態目錄。"))?;
    match protected.child(bootstrap_verify::STAGE_DIRECTORY) {
        Ok(stages) => stages.lock().map(Some).map_err(|_| refused("安裝包驗證程式正在執行，請稍後再回退。")),
        Err(duduclaw_updated::Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(refused("無法開啟安裝包暫存區。")),
    }
}

fn unit_file_present(unit: &str) -> Result<bool> {
    match fs::symlink_metadata(unit_path(unit)) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
        Ok(_) => Ok(true),
    }
}

fn stop_units() -> Result<()> {
    for unit in STOP_ORDER {
        if !unit_file_present(unit)? { continue; }
        let output = command("systemctl", &["stop", unit])?;
        if !output.status.success() { return Err(refused(format!("無法停止 {unit}。"))); }
    }
    for unit in ENABLED {
        if !unit_file_present(unit)? { continue; }
        // Removes the enablement links before the unit files go.
        let output = command("systemctl", &["disable", unit])?;
        if !output.status.success() { return Err(refused(format!("無法停用 {unit}。"))); }
    }
    for pattern in INSTANCE_PATTERNS {
        let output = command("systemctl", &["stop", pattern])?;
        if !output.status.success() { return Err(refused("無法停止隔離服務的執行個體。")); }
    }
    // Read back: every unit is inactive (unknown answers refuse).
    for unit in STOP_ORDER {
        let output = command("systemctl", &["is-active", "--quiet", unit])?;
        match output.status.code() {
            Some(3 | 4) => (),
            Some(0) => return Err(refused(format!("{unit} 仍在執行。"))),
            _ => return Err(refused("systemd 指令失敗或回應無法判讀。")),
        }
    }
    Ok(())
}

/// A root-owned regular file with one link (never a link), or absent.
fn remove_file_checked(path: &str) -> Result<()> {
    let file = Path::new(path);
    let metadata = match fs::symlink_metadata(file) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
        Ok(metadata) => metadata,
    };
    parents(file)?;
    trusted(file, false)?;
    if metadata.nlink() != 1 { return Err(refused(format!("移除目標含有硬連結：{path}"))); }
    fs::remove_file(file)?;
    let parent = file.parent().ok_or_else(|| refused("無效的移除路徑。"))?;
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}

fn remove_if_empty(path: &str) -> Result<()> {
    let directory = Path::new(path);
    match fs::symlink_metadata(directory) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
        Ok(_) => (),
    }
    parents(directory)?;
    trusted(directory, true)?;
    match fs::remove_dir(directory) {
        Ok(()) => (),
        // Not empty: something else lives there; keep it.
        Err(error) if error.raw_os_error() == Some(libc::ENOTEMPTY) || error.raw_os_error() == Some(libc::EEXIST) => (),
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

fn open_directory_at(parent: &fs::File, name: &CStr) -> Result<fs::File> {
    // SAFETY: live dirfd and a NUL-terminated single-component name.
    let fd = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(),
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC) };
    if fd < 0 { return Err(std::io::Error::last_os_error().into()); }
    // SAFETY: a fresh descriptor owned here.
    Ok(unsafe { fs::File::from_raw_fd(fd) })
}

/// Relative, no-follow removal of `name` below `parent`: never crosses a mount
/// (device must equal `device`), bounded in depth and entry count.
fn remove_entry(parent: &fs::File, name: &CStr, device: u64, depth: u32, count: &mut u32) -> Result<()> {
    if depth > 64 || *count >= MAX_TREE_ENTRIES { return Err(refused("要移除的目錄樹超過上限，已停止。")); }
    *count += 1;
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: live dirfd, NUL-terminated name, writable buffer; links are not followed.
    if unsafe { libc::fstatat(parent.as_raw_fd(), name.as_ptr(), stat.as_mut_ptr(), libc::AT_SYMLINK_NOFOLLOW) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    // SAFETY: initialized by the successful fstatat.
    let stat = unsafe { stat.assume_init() };
    #[allow(clippy::unnecessary_cast)]
    let (entry_device, inode) = (stat.st_dev as u64, stat.st_ino as u64);
    if entry_device != device { return Err(refused("要移除的目錄含有其他掛載點，已停止；請先卸載。")); }
    if stat.st_mode & libc::S_IFMT == libc::S_IFDIR {
        let child = open_directory_at(parent, name)?;
        let metadata = child.metadata()?;
        if metadata.dev() != device || metadata.ino() != inode { return Err(refused("要移除的目錄在檢查期間被更換，已停止。")); }
        for entry in boot::directory_names(&child, MAX_DIRECTORY_ENTRIES)? {
            remove_entry(&child, &entry, device, depth + 1, count)?;
        }
        drop(child);
        // SAFETY: live dirfd and a NUL-terminated single-component name.
        if unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), libc::AT_REMOVEDIR) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
    // SAFETY: live dirfd and a NUL-terminated single-component name.
    } else if unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), 0) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

/// Remove a root-owned directory tree (absent = nothing to do).
fn remove_tree(path: &str) -> Result<()> {
    let tree = Path::new(path);
    let metadata = match fs::symlink_metadata(tree) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
        Ok(metadata) => metadata,
    };
    parents(tree)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() || metadata.uid() != 0 {
        return Err(refused(format!("要移除的路徑不是 root 擁有的目錄：{path}")));
    }
    let parent_path = tree.parent().ok_or_else(|| refused("無效的移除路徑。"))?;
    let name = CString::new(tree.file_name().ok_or_else(|| refused("無效的移除路徑。"))?.as_bytes())
        .map_err(|_| refused("無效的移除路徑。"))?;
    let parent = fs::OpenOptions::new().read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC).open(parent_path)?;
    let mut count = 0;
    remove_entry(&parent, &name, metadata.dev(), 0, &mut count)?;
    parent.sync_all()?;
    Ok(())
}

/// The gateway's data written after S8 (the account's own files, possibly
/// sealed with the root key removed above): everything in the private data
/// directory except the license, removed relative to its no-follow dirfd.
fn clear_gateway_data() -> Result<()> {
    match fs::symlink_metadata(format!("{}/.duduclaw", boot::GATEWAY_HOME)) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
        Ok(_) => (),
    }
    let gateway = boot::passwd_entry("duduclaw")?.ok_or_else(|| refused("無法查詢服務帳號。"))?;
    let (data, _) = boot::open_gateway_data(Some((gateway.uid, gateway.gid)))?;
    let device = data.metadata()?.dev();
    let mut count = 0;
    for name in boot::directory_names(&data, MAX_DIRECTORY_ENTRIES)? {
        if name.as_c_str() == c"license.json" { continue; }
        remove_entry(&data, &name, device, 1, &mut count)?;
    }
    data.sync_all()?;
    Ok(())
}

/// Remove `<protected>/bootstrap/<generation>` (and an interrupted `.next`).
pub(super) fn remove_stage(generation: &str) -> Result<()> {
    if boot::parse_generation(generation).is_none() { return Err(refused("無效的世代。")); }
    let stages = format!("{}/{}", bootstrap_verify::PROTECTED_ROOT, bootstrap_verify::STAGE_DIRECTORY);
    remove_tree(&format!("{stages}/{generation}"))?;
    remove_tree(&format!("{stages}/{generation}.next"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rollback_removes_only_what_a_fresh_install_creates() {
        let files = removal_files();
        // Every isolation binary and unit file is covered; data and identities are not.
        for binary in layout::BINARIES { assert!(files.iter().any(|file| file == binary), "{binary}"); }
        for (unit, _) in layout::UNITS { assert!(files.contains(&unit_path(unit)), "{unit}"); }
        for kept in ["/var/lib/duduclaw", "/var/lib/duduclaw/.duduclaw", "/var/lib/duduclaw-protected",
            duduclaw_core::protected_mode::MODE_MARKER, "/etc/passwd", "/etc/ssh/sshd_config.d/10-duduclaw.conf"] {
            assert!(!files.iter().any(|file| file == kept) && !TREES.contains(&kept), "{kept}");
        }
        // Every isolation fact is a removed file, inside a removed tree, or a runtime socket below one.
        // The durable import manifest is written only by G's migration (R5), never by a
        // fresh install (S1 refuses when it exists), so a rollback never removes it.
        for fact in duduclaw_core::isolation_install::FACTS {
            if *fact == duduclaw_core::isolation_install::DURABLE_IMPORT_MANIFEST { continue; }
            let covered = files.iter().any(|file| file == fact)
                || TREES.iter().any(|tree| *fact == *tree || fact.strip_prefix(tree).is_some_and(|rest| rest.starts_with('/')));
            assert!(covered, "{fact}");
        }
        // The gateway stops first; sockets stop before the services they activate.
        assert_eq!(STOP_ORDER[0], UNIT);
        let position = |unit: &str| STOP_ORDER.iter().position(|item| *item == unit).unwrap();
        assert!(position("duduclaw-spawnd.socket") < position("duduclaw-spawnd.service"));
        assert!(position("duduclaw-egress.socket") < position("duduclaw-egress.service"));
    }
}
