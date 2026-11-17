//! Rollback of an uncommitted v1.83 isolation bootstrap (plan §4.3).
//!
//! Window: any time before S9 Commit of a fresh install (`committed:false`).
//! After the commit only `service uninstall --protected-linux` exists. The
//! upgrade window (until `KeysRotated`, Q8) and its un-quarantine belong to R5
//! and are refused here.
//!
//! What it removes is decided by the creation ledger (review F4,
//! `protected_linux_ledger.rs`), never by what happens to sit at a fixed path
//! now: only ledgered objects whose current identity equals the recorded one
//! are removed — regular files (units, programs, launchers, policy files,
//! version files, the root key), the trees this transaction created
//! (`/opt/duduclaw-isolated`, `/etc/duduclaw-updated`, `/var/lib/duduclaw-updated`,
//! `/var/lib/duduclaw-spawnd`, `/var/log/duduclaw-spawnd`, `/usr/lib/duduclaw`,
//! `/usr/libexec/duduclaw`, `/var/lib/duduclaw-migrate`, `/run/duduclaw-spawnd`),
//! directories it created (only when empty afterwards), and in the gateway's
//! data directory only what appeared after its S8 snapshot. Anything else at a
//! ledgered path is kept and listed; the rollback then ends incomplete: the
//! journal stays in the rollback state and the same command finishes it once
//! the administrator resolved the listed paths. Units are stopped and disabled
//! only when their ledgered unit file is the recorded one. It keeps the system
//! accounts, the gateway home (root 0711 with the private `.duduclaw` and the
//! license), the protected-mode marker (it only narrows), the bootstrap
//! high-water record (anti-rollback, never lowered) and spawnd's SSH/cron
//! lockdown entries (they only deny).
//!
//! Deletion never follows a link and never crosses a mount (review F3): every
//! tree records its root's mount id (`statx` `STATX_MNT_ID`, `AT_NO_AUTOMOUNT`,
//! no-follow), the root must sit on its parent's mount, and every entry's mount
//! id (and device) must equal it before it is descended into or unlinked. A
//! kernel that cannot report mount ids (before 5.8) refuses; there is no
//! `st_dev` fallback. Bounded; no `rm -rf`. Finally no isolation fact may remain.
//!
//! LOCK ORDER (see `protected_linux_bootstrap.rs`): 0 (the caller) → 1 (R1's
//! stage lock) → 2 (`version_lock`) → 3 (updater state, before its removal).
use super::*;
use super::bootstrap::{self as boot, Journal, Phase, Step, Store};
use super::ledger::{self, Entry, Kind, Ledger, Now};
use std::{ffi::CStr, os::fd::FromRawFd};
use duduclaw_updated::linux::{bootstrap_verify, files::Directory};

/// Stop order: the gateway (then the units it binds to), the nightly
/// requester, every socket (so nothing is re-activated), then the services.
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
/// Each is stopped only when the unit owning it is this transaction's.
const INSTANCE_PATTERNS: [(&str, &str); 4] = [("duduclaw-parse@*.service", "duduclaw-parse@.service"),
    ("duduclaw-import@*.service", "duduclaw-import@.service"), ("duduclaw-run-*.service", "duduclaw-spawnd.service"),
    ("duduclaw-migrate-expire@*.timer", "duduclaw-migrate-expire@.timer")];
/// Root-owned trees an install run creates (all isolation facts or below one).
pub(super) const TREES: [&str; 9] = ["/opt/duduclaw-isolated", "/etc/duduclaw-updated", "/var/lib/duduclaw-updated",
    "/var/lib/duduclaw-spawnd", "/var/log/duduclaw-spawnd", "/usr/lib/duduclaw", "/usr/libexec/duduclaw",
    "/var/lib/duduclaw-migrate", "/run/duduclaw-spawnd"];
/// Directories outside the trees a run may create; ledgered only when this run
/// created them, removed only when empty.
pub(super) const LEDGER_DIRECTORIES: [&str; 5] = ["/usr/local/libexec/duduclaw", "/opt/duduclaw/bin", "/opt/duduclaw",
    "/usr/local/libexec", "/usr/local/sbin"];
const MAX_TREE_ENTRIES: u32 = 200_000;
const MAX_DIRECTORY_ENTRIES: usize = 100_000;
const OTHER_MOUNT: &str = "要移除的目錄含有其他掛載點（或在檢查期間被更換），已停止回退；請先卸載後再執行同一個回退命令。";

fn unit_path(name: &str) -> String { format!("/etc/systemd/system/{name}") }

/// Every regular file a run may create (the ledger's `Kind::File` allowlist).
pub(super) fn ledger_files() -> Vec<String> {
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
    boot::require_this_host(&journal)?;
    let _stage = stage_lock()?;
    let _version = version_lock()?;
    // Without a ledger nothing was recorded as created, so nothing is removed.
    let ledger = ledger::read(store)?.unwrap_or_else(|| Ledger::new(&journal.generation));
    if ledger.generation != journal.generation { return Err(refused(ledger::LEDGER_DAMAGED)); }
    let journal = if journal.step == Step::Rollback { journal } else {
        let marked = journal.at(Step::Rollback, Phase::Entered);
        store.write(Some(&journal), &marked)?;
        marked
    };
    println!("正在回退尚未完成的隔離安裝……");
    let boot_id = ledger::boot_id()?;
    // After a reboot systemd recreated spawnd's runtime directory (tmpfs) from
    // this transaction's own socket unit: identified by that unit, not by inode.
    let runtime_by_our_unit = owned_unit(&ledger, "duduclaw-spawnd.socket", &boot_id)?;
    stop_units(&ledger, &boot_id)?;
    removal::processes_clear()?;
    let mut kept: Vec<String> = Vec::new();
    for entry in ledger.entries.iter().filter(|entry| entry.kind == Kind::File) {
        if remove_file(entry, &boot_id)? { kept.push(entry.path.clone()); }
    }
    let reload = command("systemctl", &["daemon-reload"])?;
    if !reload.status.success() { return Err(refused("systemd 指令失敗或回應無法判讀。")); }
    for entry in ledger.entries.iter().filter(|entry| entry.kind == Kind::Contents) {
        if clear_contents(entry, &boot_id)? { kept.push(entry.path.clone()); }
    }
    {
        // Lock 3 while the updater state is removed (its writers are stopped).
        let _state_lock = match Directory::open(Path::new(duduclaw_updated::linux::STATE_ROOT)) {
            Ok(state) => Some(state.lock().map_err(|_| refused("更新服務的狀態正在使用中，請稍後再回退。"))?),
            Err(duduclaw_updated::Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(_) => return Err(refused("更新服務的狀態目錄不是 root 擁有的安全目錄，已停止回退。")),
        };
        for entry in ledger.entries.iter().filter(|entry| entry.kind == Kind::Tree) {
            let recreated_at_boot = runtime_by_our_unit && entry.path == boot::SPAWND_RUNTIME
                && entry.identity.as_ref().is_none_or(|identity| identity.boot_id != boot_id);
            if remove_tree_entry(entry, &boot_id, &kept, recreated_at_boot)? { kept.push(entry.path.clone()); }
        }
    }
    // Deepest first, so a ledgered parent is empty once its ledgered children are gone.
    let mut directories: Vec<&Entry> = ledger.entries.iter().filter(|entry| entry.kind == Kind::Directory).collect();
    directories.sort_by_key(|entry| std::cmp::Reverse(Path::new(&entry.path).components().count()));
    for entry in directories {
        if remove_directory(entry, &boot_id)? { kept.push(entry.path.clone()); }
    }
    let remaining: Vec<&str> = duduclaw_core::isolation_install::FACTS.iter().copied()
        .filter(|fact| fs::symlink_metadata(fact).map(|_| true).unwrap_or_else(|error| error.kind() != std::io::ErrorKind::NotFound))
        .collect();
    finish(&kept, &remaining)?;
    remove_stage(&journal.generation)?;
    ledger::remove(store)?;
    store.remove()?;
    println!("已回退到安裝前的狀態。保留：系統帳號、空的 Gateway 資料目錄與團隊版授權、受保護模式標記，以及安裝包的防降版紀錄。");
    Ok(())
}

/// Review F4: a rollback that kept anything, or left a fact, is incomplete and
/// resumable (the journal stays in the rollback state); it never reports done.
pub(super) fn finish(kept: &[String], remaining: &[&str]) -> Result<()> {
    if kept.is_empty() && remaining.is_empty() { return Ok(()); }
    if !kept.is_empty() {
        println!("[保留] 以下路徑上的物件不是這次安裝建立的原物件（已被更換、來源不明，或目錄裡有其他內容），沒有移除：");
        for path in kept { println!("  - {path}"); }
    }
    if !remaining.is_empty() {
        println!("[未通過] 以下隔離安裝的路徑仍然存在：");
        for path in remaining { println!("  - {path}"); }
    }
    Err(refused("回退尚未完成；請確認上列路徑的來源並處理後，再執行同一個回退命令。"))
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

/// What a ledgered path holds now, relative to its entry.
fn now(entry: &Entry, boot_id: &str) -> Result<Now> {
    if !ledger::present(&entry.path)? { return Ok(Now::Absent); }
    let (directory, name) = ledger::open_parent(&entry.path)?;
    ledger::classify_at(&directory, &name, entry, boot_id)
}

/// The unit file is this transaction's, exactly as recorded.
fn owned_unit(ledger: &Ledger, unit: &str, boot_id: &str) -> Result<bool> {
    match ledger.entry(&unit_path(unit)) {
        Some(entry) => Ok(now(entry, boot_id)? == Now::Recorded),
        None => Ok(false),
    }
}

fn with_ready_units(units: &[&'static str]) -> Vec<&'static str> {
    // The units the gateway binds to stop right after it (none in this tree).
    let mut ordered = vec![UNIT];
    ordered.extend(boot::GATEWAY_READY_UNITS.iter().copied());
    ordered.extend(units.iter().copied().filter(|unit| *unit != UNIT));
    ordered
}

fn stop_units(ledger: &Ledger, boot_id: &str) -> Result<()> {
    let mut stopped = Vec::new();
    for unit in with_ready_units(&STOP_ORDER) {
        if !owned_unit(ledger, unit, boot_id)? { continue; }
        let output = command("systemctl", &["stop", unit])?;
        if !output.status.success() { return Err(refused(format!("無法停止 {unit}。"))); }
        stopped.push(unit);
    }
    for unit in with_ready_units(&ENABLED) {
        if !owned_unit(ledger, unit, boot_id)? { continue; }
        // Removes the enablement links before the unit files go.
        let output = command("systemctl", &["disable", unit])?;
        if !output.status.success() { return Err(refused(format!("無法停用 {unit}。"))); }
    }
    for (pattern, owner) in INSTANCE_PATTERNS {
        if !owned_unit(ledger, owner, boot_id)? { continue; }
        let output = command("systemctl", &["stop", pattern])?;
        if !output.status.success() { return Err(refused("無法停止隔離服務的執行個體。")); }
    }
    // Read back: every unit stopped here is inactive (unknown answers refuse).
    for unit in stopped {
        let output = command("systemctl", &["is-active", "--quiet", unit])?;
        match output.status.code() {
            Some(3 | 4) => (),
            Some(0) => return Err(refused(format!("{unit} 仍在執行。"))),
            _ => return Err(refused("systemd 指令失敗或回應無法判讀。")),
        }
    }
    Ok(())
}

/// `true` = kept (not the recorded object).
fn remove_file(entry: &Entry, boot_id: &str) -> Result<bool> {
    if !ledger::present(&entry.path)? { return Ok(false); }
    let (directory, name) = ledger::open_parent(&entry.path)?;
    remove_file_at(&directory, &name, entry, boot_id)
}

/// Unlink `name` only when it is exactly the ledgered object with one link.
/// `true` = kept.
pub(super) fn remove_file_at(directory: &fs::File, name: &CStr, entry: &Entry, boot_id: &str) -> Result<bool> {
    match ledger::classify_at(directory, name, entry, boot_id)? {
        Now::Absent => Ok(false),
        Now::Other => Ok(true),
        Now::Recorded => {
            if ledger::stat_at(directory.as_raw_fd(), name)?.nlink != 1 { return Ok(true); }
            // SAFETY: live dirfd and a NUL-terminated single-component name.
            if unsafe { libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), 0) } != 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            directory.sync_all()?;
            Ok(false)
        }
    }
}

fn remove_directory(entry: &Entry, boot_id: &str) -> Result<bool> {
    if !ledger::present(&entry.path)? { return Ok(false); }
    let (parent, name) = ledger::open_parent(&entry.path)?;
    match ledger::classify_at(&parent, &name, entry, boot_id)? {
        Now::Absent => Ok(false),
        Now::Other => Ok(true),
        Now::Recorded => {
            // SAFETY: live dirfd and a NUL-terminated single-component name.
            if unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), libc::AT_REMOVEDIR) } != 0 {
                let error = std::io::Error::last_os_error();
                // Not empty: something else lives there; keep it and report.
                if matches!(error.raw_os_error(), Some(libc::ENOTEMPTY | libc::EEXIST)) { return Ok(true); }
                return Err(error.into());
            }
            parent.sync_all()?;
            Ok(false)
        }
    }
}

/// A ledgered tree, unless a kept object lies below it. `recreated_at_boot`:
/// spawnd's runtime directory recorded in an earlier boot (or never confirmed)
/// while this transaction's own socket unit recreates it at boot; then a
/// root-owned directory there is removed (still never crossing a mount).
fn remove_tree_entry(entry: &Entry, boot_id: &str, kept: &[String], recreated_at_boot: bool) -> Result<bool> {
    if kept.iter().any(|path| path.strip_prefix(entry.path.as_str()).is_some_and(|rest| rest.starts_with('/'))) {
        return Ok(true);
    }
    if !ledger::present(&entry.path)? { return Ok(false); }
    let (parent, name) = ledger::open_parent(&entry.path)?;
    let recorded = match ledger::classify_at(&parent, &name, entry, boot_id)? {
        Now::Absent => return Ok(false),
        Now::Recorded => true,
        Now::Other => {
            let stat = ledger::stat_at(parent.as_raw_fd(), &name)?;
            recreated_at_boot && stat.mode & libc::S_IFMT == libc::S_IFDIR && stat.uid == 0 && stat.gid == 0
        }
    };
    if !recorded { return Ok(true); }
    remove_below(&parent, &name)?;
    parent.sync_all()?;
    Ok(false)
}

/// The gateway's data written after its S8 snapshot (the account's own
/// files, possibly sealed with the root key removed above). The directory and
/// the snapshot's names (the license) stay.
fn clear_contents(entry: &Entry, boot_id: &str) -> Result<bool> {
    if !ledger::present(&entry.path)? { return Ok(false); }
    let (parent, name) = ledger::open_parent(&entry.path)?;
    match ledger::classify_at(&parent, &name, entry, boot_id)? {
        Now::Absent => Ok(false),
        Now::Other => Ok(true),
        Now::Recorded => {
            let data = open_directory_at(&parent, &name)?;
            let opened = ledger::stat_at(data.as_raw_fd(), c"")?;
            if entry.identity.as_ref().is_none_or(|identity| identity.inode != opened.inode) { return Ok(true); }
            for child in boot::directory_names(&data, MAX_DIRECTORY_ENTRIES)? {
                if child.to_str().is_ok_and(|child| entry.keep.iter().any(|keep| keep == child)) { continue; }
                remove_below(&data, &child)?;
            }
            data.sync_all()?;
            Ok(false)
        }
    }
}

fn open_directory_at(parent: &fs::File, name: &CStr) -> Result<fs::File> {
    // SAFETY: live dirfd and a NUL-terminated single-component name.
    let fd = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(),
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC) };
    if fd < 0 { return Err(std::io::Error::last_os_error().into()); }
    // SAFETY: a fresh descriptor owned here.
    Ok(unsafe { fs::File::from_raw_fd(fd) })
}

/// Review F3: remove `name` below `parent` recursively. The root's mount id is
/// recorded first; the root must sit on `parent`'s mount (it is not itself a
/// mount point) and every entry must be on the root's mount and device.
pub(super) fn remove_below(parent: &fs::File, name: &CStr) -> Result<()> {
    let unavailable = || refused(ledger::MOUNT_ID_UNAVAILABLE);
    let root = ledger::stat_at(parent.as_raw_fd(), name)?;
    let mount = root.mount_id.ok_or_else(unavailable)?;
    let parent_mount = ledger::stat_at(parent.as_raw_fd(), c"")?.mount_id.ok_or_else(unavailable)?;
    if mount != parent_mount { return Err(refused(OTHER_MOUNT)); }
    let mut count = 0;
    remove_entry(parent, name, root.device, mount, 0, &mut count)
}

/// Relative, no-follow removal of `name` below `parent`, bounded in depth and
/// entry count; refuses at the first entry on another mount or device.
fn remove_entry(parent: &fs::File, name: &CStr, device: u64, mount: u64, depth: u32, count: &mut u32) -> Result<()> {
    if depth > 64 || *count >= MAX_TREE_ENTRIES { return Err(refused("要移除的目錄樹超過上限，已停止。")); }
    *count += 1;
    let stat = ledger::stat_at(parent.as_raw_fd(), name)?;
    let entry_mount = stat.mount_id.ok_or_else(|| refused(ledger::MOUNT_ID_UNAVAILABLE))?;
    if entry_mount != mount || stat.device != device { return Err(refused(OTHER_MOUNT)); }
    if stat.mode & libc::S_IFMT == libc::S_IFDIR {
        let child = open_directory_at(parent, name)?;
        let opened = ledger::stat_at(child.as_raw_fd(), c"")?;
        if opened.inode != stat.inode || opened.device != device || opened.mount_id != Some(mount) {
            return Err(refused(OTHER_MOUNT));
        }
        for entry in boot::directory_names(&child, MAX_DIRECTORY_ENTRIES)? {
            remove_entry(&child, &entry, device, mount, depth + 1, count)?;
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

/// Remove a root-owned directory tree by path (absent = nothing to do).
fn remove_tree(path: &str) -> Result<()> {
    let tree = Path::new(path);
    let metadata = match fs::symlink_metadata(tree) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
        Ok(metadata) => metadata,
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() || metadata.uid() != 0 {
        return Err(refused(format!("要移除的路徑不是 root 擁有的目錄：{path}")));
    }
    let (parent, name) = ledger::open_parent(path)?;
    remove_below(&parent, &name)?;
    parent.sync_all()?;
    Ok(())
}

/// Remove `<protected>/bootstrap/<generation>` (and an interrupted `.next`):
/// R1's stage of this bootstrap, not an installed object.
pub(super) fn remove_stage(generation: &str) -> Result<()> {
    if boot::parse_generation(generation).is_none() { return Err(refused("無效的世代。")); }
    let stages = format!("{}/{}", bootstrap_verify::PROTECTED_ROOT, bootstrap_verify::STAGE_DIRECTORY);
    remove_tree(&format!("{stages}/{generation}"))?;
    remove_tree(&format!("{stages}/{generation}.next"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::ledger::{Identity, State, MOUNT_ID_SEAM};

    #[test]
    fn rollback_removes_only_what_a_fresh_install_creates() {
        let files = ledger_files();
        // Every isolation binary and unit file is covered; data and identities are not.
        for binary in layout::BINARIES { assert!(files.iter().any(|file| file == binary), "{binary}"); }
        for (unit, _) in layout::UNITS { assert!(files.contains(&unit_path(unit)), "{unit}"); }
        for kept in ["/var/lib/duduclaw", "/var/lib/duduclaw/.duduclaw", "/var/lib/duduclaw-protected",
            duduclaw_core::protected_mode::MODE_MARKER, "/etc/passwd", "/etc/ssh/sshd_config.d/10-duduclaw.conf", "/etc/duduclaw"] {
            assert!(!files.iter().any(|file| file == kept) && !TREES.contains(&kept) && !LEDGER_DIRECTORIES.contains(&kept), "{kept}");
            for kind in [Kind::File, Kind::Directory, Kind::Tree] { assert!(!ledger::permitted(kind, kept), "{kept}"); }
        }
        // Every isolation fact is a ledgerable file, inside a ledgerable tree, or a runtime socket below one.
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
        assert_eq!(with_ready_units(&STOP_ORDER)[0], UNIT);
        let position = |unit: &str| STOP_ORDER.iter().position(|item| *item == unit).unwrap();
        assert!(position("duduclaw-spawnd.socket") < position("duduclaw-spawnd.service"));
        assert!(position("duduclaw-egress.socket") < position("duduclaw-egress.service"));
        // Every unit the gateway binds to is part of the layout (none in this tree).
        for unit in boot::GATEWAY_READY_UNITS {
            assert!(layout::UNITS.iter().any(|(name, _)| name == unit), "{unit}");
        }
    }

    fn set_mount_ids(rewrite: impl Fn(&[u8]) -> Option<u64> + 'static) {
        MOUNT_ID_SEAM.with(|seam| *seam.borrow_mut() = Some(Box::new(move |name: &CStr, _: Option<u64>| rewrite(name.to_bytes()))));
    }
    fn clear_mount_ids() { MOUNT_ID_SEAM.with(|seam| *seam.borrow_mut() = None); }

    fn tree() -> (tempfile::TempDir, fs::File) {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir_all(directory.path().join("tree/a")).unwrap();
        fs::write(directory.path().join("tree/a/file"), b"x").unwrap();
        fs::create_dir(directory.path().join("tree/b")).unwrap();
        let parent = fs::File::open(directory.path()).unwrap();
        (directory, parent)
    }

    #[test]
    fn rollback_refuses_to_cross_a_different_mount_id() {
        let (directory, parent) = tree();
        // `b` is a same-device bind mount: same st_dev, another mount id.
        set_mount_ids(|name| Some(if name == b"b" { 2 } else { 1 }));
        let result = remove_below(&parent, c"tree");
        clear_mount_ids();
        assert!(result.is_err());
        assert!(directory.path().join("tree/b").is_dir(), "the other mount is never entered or removed");
        // A tree root that is itself a mount point is refused before anything is removed.
        let (directory, parent) = tree();
        set_mount_ids(|name| Some(if name == b"tree" { 2 } else { 1 }));
        let result = remove_below(&parent, c"tree");
        clear_mount_ids();
        assert!(result.is_err());
        assert!(directory.path().join("tree/a/file").is_file());
        // One mount throughout: removed.
        let (directory, parent) = tree();
        set_mount_ids(|_| Some(1));
        let result = remove_below(&parent, c"tree");
        clear_mount_ids();
        result.unwrap();
        assert!(!directory.path().join("tree").exists());
    }

    #[test]
    fn rollback_refuses_without_kernel_mount_ids() {
        let (directory, parent) = tree();
        // Pre-5.8 kernels: no STATX_MNT_ID. Never fall back to st_dev.
        set_mount_ids(|_| None);
        let result = remove_below(&parent, c"tree");
        clear_mount_ids();
        assert!(result.is_err());
        assert!(directory.path().join("tree/a/file").is_file() && directory.path().join("tree/b").is_dir());
        // Only the entries' answers missing: still refused, at the first one.
        let (directory, parent) = tree();
        set_mount_ids(|name| if name.is_empty() || name == b"tree" { Some(1) } else { None });
        let result = remove_below(&parent, c"tree");
        clear_mount_ids();
        assert!(result.is_err());
        assert!(directory.path().join("tree").is_dir());
    }

    #[test]
    fn rollback_keeps_object_whose_identity_differs_and_ends_incomplete() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("unit");
        fs::write(&path, b"ours").unwrap();
        let dir = fs::File::open(directory.path()).unwrap();
        let boot_id = ledger::boot_id().unwrap();
        let recorded: Identity = ledger::identity_at(&dir, c"unit", Kind::File).unwrap().unwrap();
        let entry = Entry { path: UNIT_PATH.into(), kind: Kind::File, state: State::Created, identity: Some(recorded),
            keep: Vec::new() };
        // A package or an administrator replaced it after the interruption.
        fs::write(directory.path().join("unit.pkg"), b"theirs").unwrap();
        fs::rename(directory.path().join("unit.pkg"), &path).unwrap();
        assert!(remove_file_at(&dir, c"unit", &entry, &boot_id).unwrap(), "kept");
        assert_eq!(fs::read(&path).unwrap(), b"theirs");
        assert!(finish(&[entry.path.clone()], &[]).is_err(), "an incomplete rollback never reports done");
        // An intent without an identity matches nothing either.
        let intent = Entry { identity: None, state: State::Intent, ..entry.clone() };
        assert!(remove_file_at(&dir, c"unit", &intent, &boot_id).unwrap());
        assert!(path.exists());
        // The recorded object itself is removed; then the path is simply absent.
        let ours = Entry { identity: ledger::identity_at(&dir, c"unit", Kind::File).unwrap(), ..entry.clone() };
        assert!(!remove_file_at(&dir, c"unit", &ours, &boot_id).unwrap());
        assert!(!path.exists());
        assert!(!remove_file_at(&dir, c"unit", &ours, &boot_id).unwrap());
        // A hard-linked object is kept even when it is the recorded inode.
        fs::write(&path, b"ours").unwrap();
        let linked = Entry { identity: ledger::identity_at(&dir, c"unit", Kind::File).unwrap(), ..entry };
        fs::hard_link(&path, directory.path().join("alias")).unwrap();
        assert!(remove_file_at(&dir, c"unit", &linked, &boot_id).unwrap());
        finish(&[], &[]).unwrap();
        assert!(finish(&[], &["/usr/lib/duduclaw"]).is_err());
    }
}
