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
//! Review M1: a ledgered object is never unlinked where it was checked. It is
//! first moved (`renameat2(RENAME_NOREPLACE)`) into a fresh root-only (0700)
//! quarantine directory in its own parent (same filesystem; a parent that is
//! not root-owned or is writable by others refuses), its identity is verified
//! again there by `statx`/fd with the same rules (mount id included), and only
//! then is it removed inside the quarantine — a tree root likewise, then
//! walked there with the mount-bounded logic. Anything that no longer matches
//! is moved back (or, if that fails, left in the quarantine), kept, listed,
//! and the rollback ends incomplete.
//!
//! Review R3 item 5: the quarantine is durable. Its name is deterministic per
//! entry (`ledger::quarantine_name`: transaction nonce and entry index; still
//! created with exclusive `mkdirat` 0700) and is written into the ledger entry
//! with the phase `quarantining` BEFORE the directory is created and the
//! object moved, then `removed` once the quarantine holds nothing of it. Every
//! rollback start and every install resume first reconciles: a recorded
//! quarantine directory that exists is finished (identity re-verified inside;
//! removed when it is the recorded object, otherwise moved back without
//! replacing, or kept and listed). The journal and the ledger are deleted only
//! when no recorded quarantine directory remains, and S1 refuses a host with a
//! quarantine directory no ledger explains.
//!
//! Review M3: objects under `/run` recorded in an earlier boot are never
//! deleted (the boot cleared what this transaction created; whatever is there
//! now is someone else's). They are reported as left to the system and do not
//! make the rollback incomplete once the units owning them were removed by
//! this rollback; if those units could not be removed it stays incomplete.
//!
//! LOCK ORDER (see `protected_linux_bootstrap.rs`): 0 (the caller) → 1 (R1's
//! stage lock) → 2 (`version_lock`) → 3 (updater state, before its removal).
use super::*;
use super::bootstrap::{self as boot, Journal, Phase, Step, Store};
use super::ledger::{self, Entry, Kind, Ledger, Now, QuarantinePhase};
use std::{ffi::{CStr, CString}, os::fd::FromRawFd};
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
/// Review M3: the units whose runtime directory lives under `/run`.
static RUNTIME_OWNERS: [(&str, [&str; 2]); 1] =
    [(boot::SPAWND_RUNTIME, ["duduclaw-spawnd.socket", "duduclaw-spawnd.service"])];
/// Owner of every parent a quarantine is created in (production: root).
const ROOT_OWNER: u32 = 0;

#[cfg(test)]
thread_local! {
    /// Unit-test seam: runs after the first identity check, before the move
    /// into the quarantine (a concurrent rename). Never set in production.
    static AFTER_FIRST_CHECK: std::cell::RefCell<Option<Box<dyn FnMut()>>> = const { std::cell::RefCell::new(None) };
    /// Unit-test seam (review R3 item 5): stop right after the move into the
    /// quarantine, as a power loss would. Never set in production.
    static STOP_AFTER_RENAME: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

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
    // A ledger of another transaction (generation or nonce) is refused (review H3).
    let mut ledger = ledger::read(store)?.unwrap_or_else(|| Ledger::new(&journal.generation, &journal.nonce));
    ledger::belongs_to(&ledger, &journal.generation, &journal.nonce)?;
    let boot_id = ledger::boot_id()?;
    // Review R3 item 5: an interrupted quarantine is finished before anything else.
    let mut kept: Vec<String> = reconcile(store, &mut ledger, &boot_id)?;
    let journal = if journal.step == Step::Rollback { journal } else {
        let marked = journal.at(Step::Rollback, Phase::Entered);
        store.write(Some(&journal), &marked)?;
        marked
    };
    println!("正在回退尚未完成的隔離安裝……");
    stop_units(&ledger, &boot_id)?;
    removal::processes_clear()?;
    // Review M3: runtime directories of an earlier boot, left to the system.
    let mut left: Vec<String> = Vec::new();
    for index in indices(&ledger, Kind::File) {
        if let Removal::Kept(path) = remove_indexed(store, &mut ledger, index, &boot_id)? { kept.push(path); }
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
        for index in indices(&ledger, Kind::Tree) {
            let entry = ledger.entries.get(index).cloned().ok_or_else(|| refused(ledger::LEDGER_DAMAGED))?;
            if from_earlier_boot(&entry, &boot_id) {
                // Review M3: never deleted; left to the system once its units are gone.
                let mut owners_removed = runtime_owners(&entry.path).is_some();
                for unit in runtime_owners(&entry.path).unwrap_or_default() {
                    if ledger::present(&unit_path(unit))? { owners_removed = false; }
                }
                if owners_removed { left.push(entry.path.clone()); } else { kept.push(entry.path.clone()); }
                continue;
            }
            // A kept object lies below it: the tree stays.
            if kept.iter().any(|path| path.strip_prefix(entry.path.as_str()).is_some_and(|rest| rest.starts_with('/'))) {
                kept.push(entry.path.clone());
                continue;
            }
            if let Removal::Kept(path) = remove_indexed(store, &mut ledger, index, &boot_id)? { kept.push(path); }
        }
    }
    // Deepest first, so a ledgered parent is empty once its ledgered children are gone.
    let mut directories = indices(&ledger, Kind::Directory);
    directories.sort_by_key(|index| std::cmp::Reverse(ledger.entries.get(*index)
        .map_or(0, |entry| Path::new(&entry.path).components().count())));
    for index in directories {
        if let Removal::Kept(path) = remove_indexed(store, &mut ledger, index, &boot_id)? { kept.push(path); }
    }
    let remaining = remaining_facts(|fact| fs::symlink_metadata(fact).map(|_| true)
        .unwrap_or_else(|error| error.kind() != std::io::ErrorKind::NotFound), &left);
    report_left(&left);
    finish(&kept, &remaining)?;
    // Review R3 item 5: never delete the records while a quarantine directory remains.
    records_deletable(&outstanding_quarantines(&ledger)?)?;
    remove_stage(&journal.generation)?;
    ledger::remove(store)?;
    store.remove()?;
    println!("已回退到安裝前的狀態。保留：系統帳號、空的 Gateway 資料目錄與團隊版授權、受保護模式標記，以及安裝包的防降版紀錄。");
    Ok(())
}

/// Review M3 (pure): a ledgered object under `/run` whose recorded identity
/// was observed in another boot. That boot's tmpfs is gone; what is there now
/// was not created by this transaction and is never deleted by rollback.
pub(super) fn from_earlier_boot(entry: &Entry, boot_id: &str) -> bool {
    entry.path.starts_with("/run/") && entry.identity.as_ref().is_some_and(|identity| identity.boot_id != boot_id)
}

/// The units owning a ledgered runtime directory (`None`: not a known one).
pub(super) fn runtime_owners(path: &str) -> Option<&'static [&'static str]> {
    RUNTIME_OWNERS.iter().find(|(runtime, _)| *runtime == path).map(|(_, units)| units.as_slice())
}

/// The isolation facts still present, except those below a runtime directory
/// left to the system (review M3).
pub(super) fn remaining_facts(present: impl Fn(&str) -> bool, left: &[String]) -> Vec<&'static str> {
    duduclaw_core::isolation_install::FACTS.iter().copied()
        .filter(|fact| !left.iter().any(|tree| fact.strip_prefix(tree.as_str()).is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))))
        .filter(|fact| present(*fact)).collect()
}

fn report_left(left: &[String]) {
    if left.is_empty() { return; }
    println!("[交由系統處理] 以下執行期目錄是在先前的開機中建立的，回退不會刪除（開機時或由 systemd 清除）；擁有它們的服務單元已由這次回退移除：");
    for path in left { println!("  - {path}：runtime directory left to the system (cleared at boot / by systemd)"); }
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

/// Review R3 item 1: every unit this installation may own, in stop order
/// (the gateway, the units it binds to, the nightly requester, every socket
/// before the services, the employees' slice) — also the bootstrap's resume quiesce.
pub(super) fn stop_order() -> Vec<&'static str> { with_ready_units(&STOP_ORDER) }

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

/// What a ledgered removal did.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Removal {
    Absent,
    Removed,
    /// Kept (not the recorded object, not removable, or left in a quarantine): the path to list.
    Kept(String),
}

/// The indices of the ledger's entries of `kind`, in ledger order.
fn indices(ledger: &Ledger, kind: Kind) -> Vec<usize> {
    ledger.entries.iter().enumerate().filter(|(_, entry)| entry.kind == kind).map(|(index, _)| index).collect()
}

/// Review R3 item 5: durably record the quarantine phase of entry `index`
/// (its deterministic name included) in the ledger, read back.
fn set_quarantine(store: &Store, ledger: &mut Ledger, index: usize, phase: QuarantinePhase) -> Result<()> {
    let name = ledger::quarantine_name(&ledger.nonce, index);
    let entry = ledger.entries.get_mut(index).ok_or_else(|| refused(ledger::LEDGER_DAMAGED))?;
    entry.quarantine = Some(ledger::QuarantineRecord { name, phase });
    ledger::write(store, ledger)
}

/// The deterministic quarantine name of entry `index`, as a C string.
fn quarantine_c_name(ledger: &Ledger, index: usize) -> Result<CString> {
    CString::new(ledger::quarantine_name(&ledger.nonce, index)).map_err(|_| refused(ledger::LEDGER_DAMAGED))
}

/// A ledgered file, directory or tree (entry `index`) by its fixed path,
/// through its durable quarantine.
fn remove_indexed(store: &Store, ledger: &mut Ledger, index: usize, boot_id: &str) -> Result<Removal> {
    let entry = ledger.entries.get(index).cloned().ok_or_else(|| refused(ledger::LEDGER_DAMAGED))?;
    if !ledger::present(&entry.path)? { return Ok(Removal::Absent); }
    let (parent, name) = ledger::open_parent(&entry.path)?;
    let quarantine = quarantine_c_name(ledger, index)?;
    remove_ledgered_at(&parent, &name, &entry, boot_id, ROOT_OWNER, &quarantine,
        &mut |phase| set_quarantine(store, ledger, index, phase))
}

/// The directory below which `entry`'s quarantine lives (its parent).
fn quarantine_parent(entry: &Entry) -> Option<&str> { Path::new(&entry.path).parent()?.to_str() }

/// Review R3 item 5: finish every recorded quarantine whose directory still
/// exists (see [`reconcile_at`]). Returns the paths to list as kept.
pub(super) fn reconcile(store: &Store, ledger: &mut Ledger, boot_id: &str) -> Result<Vec<String>> {
    let mut kept = Vec::new();
    for index in 0..ledger.entries.len() {
        let Some(entry) = ledger.entries.get(index).cloned() else { continue };
        if entry.quarantine.is_none() { continue; }
        let parent = quarantine_parent(&entry).ok_or_else(|| refused(ledger::LEDGER_DAMAGED))?;
        // Without its parent there is no quarantine left either.
        if !ledger::present(parent)? { continue; }
        let (parent, name) = ledger::open_parent(&entry.path)?;
        let quarantine = quarantine_c_name(ledger, index)?;
        if let Removal::Kept(path) = reconcile_at(&parent, &name, &quarantine, &entry, boot_id, ROOT_OWNER,
            &mut |phase| set_quarantine(store, ledger, index, phase))? {
            kept.push(path);
        }
    }
    Ok(kept)
}

/// Review R3 item 5: on an install resume, a recorded quarantine is finished
/// first; when anything stays kept or in a quarantine the resume refuses
/// (only `duduclaw-bootstrap rollback` continues). `Ok(true)`: the ledger was
/// written (the caller re-reads it).
pub(super) fn reconcile_on_resume(store: &Store, recorded: &Ledger) -> Result<bool> {
    if recorded.entries.iter().all(|entry| entry.quarantine.is_none()) { return Ok(false); }
    let _version = version_lock()?;
    let mut ledger = recorded.clone();
    let kept = reconcile(store, &mut ledger, &ledger::boot_id()?)?;
    let outstanding = outstanding_quarantines(&ledger)?;
    if kept.is_empty() && outstanding.is_empty() { return Ok(true); }
    println!("[保留] 先前回退的隔離目錄無法完成處理：");
    for path in kept.iter().chain(outstanding.iter()) { println!("  - {path}"); }
    Err(refused("這台主機有尚未完成的回退隔離目錄；請以 duduclaw-bootstrap rollback 完成回退，不能續接安裝。"))
}

/// Review R3 item 5: every recorded quarantine directory still present.
pub(super) fn outstanding_quarantines(ledger: &Ledger) -> Result<Vec<String>> {
    let mut outstanding = Vec::new();
    for entry in &ledger.entries {
        let (Some(record), Some(parent)) = (&entry.quarantine, quarantine_parent(entry)) else { continue };
        let path = format!("{parent}/{}", record.name);
        if ledger::present(&path)? { outstanding.push(path); }
    }
    Ok(outstanding)
}

/// Review R3 item 5 (pure): the journal and the ledger may be deleted only
/// when no quarantine directory is outstanding.
pub(super) fn records_deletable(outstanding: &[String]) -> Result<()> {
    if outstanding.is_empty() { return Ok(()); }
    println!("[保留] 以下回退隔離目錄仍然存在，安裝紀錄不會刪除：");
    for path in outstanding { println!("  - {path}"); }
    Err(refused("回退尚未完成：仍有回退隔離目錄；請確認上列目錄的內容並處理後，再執行同一個回退命令。"))
}

/// Every directory a ledgered object (and so its quarantine) can sit in.
fn quarantine_parents() -> Vec<String> {
    let files = ledger_files();
    let mut parents: Vec<String> = files.iter().map(String::as_str).chain(TREES).chain(LEDGER_DIRECTORIES)
        .filter_map(|path| Path::new(path).parent()?.to_str().map(str::to_owned)).collect();
    parents.sort();
    parents.dedup();
    parents
}

/// Review R3 item 5 (pure): of the quarantine directories `found` (parent,
/// name), those no entry of `ours` records (same parent, same name).
pub(super) fn unexplained(found: &[(String, String)], ours: Option<&Ledger>) -> Vec<String> {
    found.iter().filter(|(parent, name)| !ours.is_some_and(|ledger| ledger.entries.iter().any(|entry|
        entry.quarantine.as_ref().is_some_and(|record| record.name == *name)
            && quarantine_parent(entry) == Some(parent.as_str()))))
        .map(|(parent, name)| format!("{parent}/{name}")).collect()
}

/// Review R3 item 5 (S1): quarantine directories (`.duduclaw-rollback-*`) in
/// any directory a ledgered object can sit in that `ours` does not explain.
pub(super) fn unexplained_quarantines(ours: Option<&Ledger>) -> Result<Vec<String>> {
    use std::os::unix::ffi::OsStrExt;
    let mut found = Vec::new();
    for parent in quarantine_parents() {
        let entries = match fs::read_dir(&parent) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        for (count, entry) in entries.enumerate() {
            if count >= MAX_DIRECTORY_ENTRIES { return Err(refused("目錄項目超過上限。")); }
            let name = entry?.file_name();
            if name.as_bytes().starts_with(ledger::QUARANTINE_PREFIX.as_bytes()) {
                found.push((parent.clone(), name.to_string_lossy().into_owned()));
            }
        }
    }
    Ok(unexplained(&found, ours))
}

fn rename_noreplace(from: &fs::File, old: &CStr, to: &fs::File, new: &CStr) -> std::io::Result<()> {
    // SAFETY: live dirfds and NUL-terminated single-component names; never replaces.
    if unsafe { libc::syscall(libc::SYS_renameat2, from.as_raw_fd(), old.as_ptr(), to.as_raw_fd(), new.as_ptr(),
        libc::RENAME_NOREPLACE) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// Review M1: a fresh root-only directory in `parent`, named deterministically
/// per ledger entry (review R3 item 5).
struct Quarantine { name: CString, directory: fs::File }

impl Quarantine {
    /// `parent` must be a directory owned by `owner` and not writable by group
    /// or others; the quarantine `name` is created 0700 there with exclusive
    /// `mkdirat` (same filesystem) and proven to be a fresh owner-only
    /// directory on the parent's mount.
    fn create(parent: &fs::File, owner: u32, path: &str, name: &CStr) -> Result<Self> {
        let unavailable = || refused(ledger::MOUNT_ID_UNAVAILABLE);
        let unsafe_parent = || refused(format!("{path} 的上層目錄不是 root 擁有的目錄，或可被其他帳號寫入；無法安全地移除它，已停止回退。"));
        let stat = ledger::stat_at(parent.as_raw_fd(), c"")?;
        if stat.mode & libc::S_IFMT != libc::S_IFDIR || stat.uid != owner || stat.mode & 0o022 != 0 {
            return Err(unsafe_parent());
        }
        let parent_mount = stat.mount_id.ok_or_else(unavailable)?;
        let name = name.to_owned();
        // SAFETY: live dirfd and a NUL-terminated single-component name.
        if unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o700) } != 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::EEXIST) {
                return Err(refused(format!("{path} 的回退隔離目錄 {} 已存在（先前的回退未完成）；請確認其內容並處理後，再執行同一個回退命令。",
                    name.to_string_lossy())));
            }
            return Err(error.into());
        }
        let directory = match open_directory_at(parent, &name) {
            Ok(directory) => directory,
            Err(error) => {
                // SAFETY: live dirfd and a NUL-terminated single-component name.
                unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), libc::AT_REMOVEDIR); }
                return Err(error);
            }
        };
        let quarantine = Self { name, directory };
        // SAFETY: scalar arguments on a live descriptor (umask may have narrowed the mode).
        let chmod = unsafe { libc::fchmod(quarantine.directory.as_raw_fd(), 0o700) } == 0;
        let created = ledger::stat_at(quarantine.directory.as_raw_fd(), c"")?;
        if !chmod || created.mode & 0o7777 != 0o700 || created.uid != owner || created.device != stat.device
            || created.mode & libc::S_IFMT != libc::S_IFDIR || created.mount_id.ok_or_else(unavailable)? != parent_mount {
            quarantine.discard(parent);
            return Err(unsafe_parent());
        }
        Ok(quarantine)
    }

    /// Remove the quarantine directory when it is empty; otherwise it stays.
    fn discard(&self, parent: &fs::File) -> bool { discard_at(parent, &self.name) }
}

/// Remove the quarantine directory `name` below `parent` when it is empty.
fn discard_at(parent: &fs::File, name: &CStr) -> bool {
    // SAFETY: live dirfd and a NUL-terminated single-component name.
    let removed = unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), libc::AT_REMOVEDIR) } == 0;
    let _ = parent.sync_all();
    removed
}

/// How a kept object left inside its quarantine is listed.
fn in_quarantine(entry: &Entry, quarantine: &CStr) -> String {
    let directory = Path::new(&entry.path).parent().map(|path| path.display().to_string()).unwrap_or_default();
    format!("{}（已移到隔離目錄 {directory}/{}/ 內）", entry.path, quarantine.to_string_lossy())
}

/// Review M1: remove the ledgered object `name` below `parent` (whose
/// owner must be `owner`: root in production) only through a quarantine.
/// First check → record the quarantine durably (`persist`, review R3 item 5)
/// → create it (`quarantine`, exclusive) and move the object into it without
/// replacing → verify the moved object again there (same rule, mount id
/// included; one link for files) → remove it inside the quarantine → record
/// `removed` → discard the empty quarantine. On any mismatch it is moved back
/// without replacing (left in the quarantine, still `quarantining`, if that fails) and kept.
pub(super) fn remove_ledgered_at(parent: &fs::File, name: &CStr, entry: &Entry, boot_id: &str, owner: u32,
    quarantine: &CStr, persist: &mut dyn FnMut(QuarantinePhase) -> Result<()>) -> Result<Removal> {
    match ledger::classify_at(parent, name, entry, boot_id)? {
        Now::Absent => return Ok(Removal::Absent),
        Now::Other => return Ok(Removal::Kept(entry.path.clone())),
        Now::Recorded => (),
    }
    if entry.kind == Kind::Contents { return Err(refused(ledger::LEDGER_DAMAGED)); }
    #[cfg(test)]
    AFTER_FIRST_CHECK.with(|hook| if let Some(hook) = hook.borrow_mut().as_mut() { hook() });
    // Review R3 item 5: durable before the quarantine exists and the object moves.
    persist(QuarantinePhase::Quarantining)?;
    let quarantine = Quarantine::create(parent, owner, &entry.path, quarantine)?;
    if let Err(error) = rename_noreplace(parent, name, &quarantine.directory, name) {
        quarantine.discard(parent);
        persist(QuarantinePhase::Removed)?;
        if error.raw_os_error() == Some(libc::ENOENT) { return Ok(Removal::Absent); }
        return Err(error.into());
    }
    #[cfg(test)]
    if STOP_AFTER_RENAME.with(|stop| stop.replace(false)) { return Err(refused("test interruption")); }
    let removed = remove_inside(&quarantine.directory, name, entry, boot_id);
    settle(parent, name, entry, &quarantine.name, &quarantine.directory, removed, persist)
}

/// Review R3 item 5: finish an interrupted quarantine `quarantine` of `entry`
/// (object `name`) below `parent`. Absent: nothing outstanding. A directory
/// that is not an owner-only (`owner`) directory on the parent's mount is
/// never touched (kept, listed). Inside it the object is verified again:
/// removed when it is the recorded one, otherwise moved back without
/// replacing (kept), or left there (kept, still outstanding).
pub(super) fn reconcile_at(parent: &fs::File, name: &CStr, quarantine: &CStr, entry: &Entry, boot_id: &str, owner: u32,
    persist: &mut dyn FnMut(QuarantinePhase) -> Result<()>) -> Result<Removal> {
    match ledger::stat_at(parent.as_raw_fd(), quarantine) {
        Ok(_) => (),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Removal::Absent),
        Err(error) => return Err(error.into()),
    }
    let inside = open_directory_at(parent, quarantine)?;
    let made = ledger::stat_at(inside.as_raw_fd(), c"")?;
    let parent_mount = ledger::stat_at(parent.as_raw_fd(), c"")?.mount_id;
    if made.mode & libc::S_IFMT != libc::S_IFDIR || made.uid != owner || made.mode & 0o7777 != 0o700
        || made.mount_id.is_none() || made.mount_id != parent_mount {
        return Ok(Removal::Kept(in_quarantine(entry, quarantine)));
    }
    if ledger::classify_at(&inside, name, entry, boot_id)? == Now::Absent {
        persist(QuarantinePhase::Removed)?;
        drop(inside);
        if discard_at(parent, quarantine) { return Ok(Removal::Absent); }
        return Ok(Removal::Kept(in_quarantine(entry, quarantine)));
    }
    let removed = remove_inside(&inside, name, entry, boot_id);
    settle(parent, name, entry, quarantine, &inside, removed, persist)
}

/// The moved object `name` inside the quarantine `inside`, verified again and
/// removed when it is the recorded one. `Ok(false)`: not the recorded object,
/// or a directory that is not empty.
fn remove_inside(inside: &fs::File, name: &CStr, entry: &Entry, boot_id: &str) -> Result<bool> {
    // An error here still moves the object back (then is reported).
    let same = (|| -> Result<bool> {
        Ok(ledger::classify_at(inside, name, entry, boot_id)? == Now::Recorded
            && (entry.kind != Kind::File || ledger::stat_at(inside.as_raw_fd(), name)?.nlink == 1))
    })();
    match (same, entry.kind) {
        (Err(error), _) => Err(error),
        (Ok(false), _) => Ok(false),
        // SAFETY: live dirfd and a NUL-terminated single-component name.
        (Ok(true), Kind::File) => if unsafe { libc::unlinkat(inside.as_raw_fd(), name.as_ptr(), 0) } == 0 { Ok(true) }
            else { Err(std::io::Error::last_os_error().into()) },
        // SAFETY: live dirfd and a NUL-terminated single-component name.
        (Ok(true), Kind::Directory) => if unsafe { libc::unlinkat(inside.as_raw_fd(), name.as_ptr(), libc::AT_REMOVEDIR) } == 0 {
            Ok(true)
        } else {
            let error = std::io::Error::last_os_error();
            // Not empty: something else lives there; keep it and report.
            if matches!(error.raw_os_error(), Some(libc::ENOTEMPTY | libc::EEXIST)) { Ok(false) } else { Err(error.into()) }
        },
        (Ok(true), Kind::Tree) => remove_below(inside, name).map(|()| true),
        (Ok(true), Kind::Contents) => Err(refused(ledger::LEDGER_DAMAGED)),
    }
}

/// After [`remove_inside`]: removed → record `removed`, discard the
/// quarantine; otherwise back where it was, never replacing, then record
/// `removed` and discard — or, when that fails, left inside (the record stays
/// `quarantining`: outstanding) and listed.
fn settle(parent: &fs::File, name: &CStr, entry: &Entry, quarantine: &CStr, inside: &fs::File, removed: Result<bool>,
    persist: &mut dyn FnMut(QuarantinePhase) -> Result<()>) -> Result<Removal> {
    if matches!(removed, Ok(true)) {
        inside.sync_all()?;
        persist(QuarantinePhase::Removed)?;
        discard_at(parent, quarantine);
        return Ok(Removal::Removed);
    }
    // Not the recorded object (or not removable): back where it was, never replacing.
    let restored = rename_noreplace(inside, name, parent, name).is_ok();
    let _ = parent.sync_all();
    let kept = if restored {
        persist(QuarantinePhase::Removed)?;
        discard_at(parent, quarantine);
        entry.path.clone()
    } else {
        in_quarantine(entry, quarantine)
    };
    removed?;
    Ok(Removal::Kept(kept))
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

    /// The test user owns the temporary parents (production: root).
    fn me() -> u32 { unsafe { libc::geteuid() } }

    /// Every name in `directory` (a quarantine left behind would show here).
    fn names(directory: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(directory).unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap()).collect();
        names.sort();
        names
    }

    fn recorded_entry(dir: &fs::File, name: &CStr, kind: Kind) -> Entry {
        let path = if kind == Kind::File { UNIT_PATH } else { "/opt/duduclaw" };
        Entry { path: path.into(), kind, state: State::Created,
            identity: ledger::identity_at(dir, name, kind).unwrap(), keep: Vec::new(), quarantine: None }
    }

    const QUARANTINE: &CStr = c".duduclaw-rollback-test-0";

    /// `remove_ledgered_at` with a fixed quarantine name, persisting nothing.
    fn remove_quarantined(dir: &fs::File, name: &CStr, entry: &Entry, boot_id: &str, owner: u32) -> Result<Removal> {
        remove_ledgered_at(dir, name, entry, boot_id, owner, QUARANTINE, &mut |_| Ok(()))
    }

    #[test]
    fn crash_after_rename_is_reconciled_before_anything_else() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir_all(directory.path().join("tree/a")).unwrap();
        fs::write(directory.path().join("tree/a/file"), b"x").unwrap();
        let dir = fs::File::open(directory.path()).unwrap();
        let boot_id = ledger::boot_id().unwrap();
        let tree = Entry { path: "/usr/lib/duduclaw".into(), kind: Kind::Tree, state: State::Created,
            identity: ledger::identity_at(&dir, c"tree", Kind::Tree).unwrap(), keep: Vec::new(), quarantine: None };
        // Power loss right after the move: the phase was durable BEFORE it.
        let mut phases = Vec::new();
        STOP_AFTER_RENAME.with(|stop| stop.set(true));
        let crashed = remove_ledgered_at(&dir, c"tree", &tree, &boot_id, me(), QUARANTINE,
            &mut |phase| { phases.push(phase); Ok(()) });
        assert!(crashed.is_err());
        assert_eq!(phases, [QuarantinePhase::Quarantining]);
        // The original path is absent; the complete tree sits in the recorded quarantine.
        assert_eq!(names(directory.path()), vec![QUARANTINE.to_str().unwrap().to_owned()]);
        assert!(directory.path().join(QUARANTINE.to_str().unwrap()).join("tree/a/file").is_file());
        // While it is outstanding the journal and ledger are never deleted.
        let outstanding = vec![directory.path().join(QUARANTINE.to_str().unwrap()).display().to_string()];
        assert!(records_deletable(&outstanding).is_err());
        // The next rollback reconciles first: identity re-verified inside, removed, `removed` recorded.
        let mut phases = Vec::new();
        let result = reconcile_at(&dir, c"tree", QUARANTINE, &tree, &boot_id, me(), &mut |phase| { phases.push(phase); Ok(()) });
        assert_eq!(result.unwrap(), Removal::Removed);
        assert_eq!(phases, [QuarantinePhase::Removed]);
        assert!(names(directory.path()).is_empty(), "tree and quarantine gone");
        records_deletable(&[]).unwrap();
        // Reconciling again (or a quarantine never created): nothing outstanding.
        assert_eq!(reconcile_at(&dir, c"tree", QUARANTINE, &tree, &boot_id, me(), &mut |_| Ok(())).unwrap(), Removal::Absent);
    }

    #[test]
    fn reconcile_moves_back_or_keeps_what_is_not_the_recorded_object() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("unit"), b"ours").unwrap();
        let dir = fs::File::open(directory.path()).unwrap();
        let boot_id = ledger::boot_id().unwrap();
        let entry = recorded_entry(&dir, c"unit", Kind::File);
        STOP_AFTER_RENAME.with(|stop| stop.set(true));
        assert!(remove_quarantined(&dir, c"unit", &entry, &boot_id, me()).is_err());
        let quarantine = directory.path().join(QUARANTINE.to_str().unwrap());
        // Someone replaced the object inside the quarantine meanwhile: moved back, kept, listed.
        fs::write(quarantine.join("unit.other"), b"theirs").unwrap();
        fs::rename(quarantine.join("unit.other"), quarantine.join("unit")).unwrap();
        let result = reconcile_at(&dir, c"unit", QUARANTINE, &entry, &boot_id, me(), &mut |_| Ok(()));
        assert_eq!(result.unwrap(), Removal::Kept(UNIT_PATH.into()));
        assert_eq!(fs::read(directory.path().join("unit")).unwrap(), b"theirs");
        assert_eq!(names(directory.path()), vec!["unit".to_owned()]);
        // A same-named directory that is not an owner-only quarantine is never touched.
        use std::os::unix::fs::PermissionsExt;
        fs::create_dir(&quarantine).unwrap();
        fs::set_permissions(&quarantine, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(quarantine.join("unit"), b"third").unwrap();
        let result = reconcile_at(&dir, c"unit", QUARANTINE, &entry, &boot_id, me(), &mut |_| Ok(()));
        assert!(matches!(result.unwrap(), Removal::Kept(_)));
        assert!(quarantine.join("unit").is_file());
        // The original path is occupied as well: it cannot go back, so it stays in the quarantine (outstanding).
        fs::set_permissions(&quarantine, fs::Permissions::from_mode(0o700)).unwrap();
        let mut phases = Vec::new();
        let result = reconcile_at(&dir, c"unit", QUARANTINE, &entry, &boot_id, me(), &mut |phase| { phases.push(phase); Ok(()) });
        assert!(matches!(result.unwrap(), Removal::Kept(listed) if listed.contains(QUARANTINE.to_str().unwrap())));
        assert!(phases.is_empty(), "still quarantining");
        assert!(quarantine.join("unit").is_file());
        assert!(records_deletable(&[quarantine.display().to_string()]).is_err());
    }

    #[test]
    fn unexplained_quarantine_directory_makes_s1_refuse() {
        let nonce = "5a".repeat(32);
        let mut ours = Ledger::new(&"cd".repeat(32), &nonce);
        ours.entries.push(Entry { path: UNIT_PATH.into(), kind: Kind::File, state: State::Created, identity: None,
            keep: Vec::new(), quarantine: Some(ledger::QuarantineRecord { name: ledger::quarantine_name(&nonce, 0),
                phase: QuarantinePhase::Removed }) });
        let explained = ("/etc/systemd/system".to_owned(), ledger::quarantine_name(&nonce, 0));
        // An old random-named quarantine, another index, another parent: unexplained.
        let found = vec![explained.clone(), ("/etc/systemd/system".to_owned(), format!("{}{}", ledger::QUARANTINE_PREFIX, "ab".repeat(16))),
            ("/etc/systemd/system".to_owned(), ledger::quarantine_name(&nonce, 1)), ("/usr/local/sbin".to_owned(), ledger::quarantine_name(&nonce, 0))];
        assert_eq!(unexplained(&found, Some(&ours)).len(), 3);
        assert_eq!(unexplained(&[explained.clone()], Some(&ours)), Vec::<String>::new());
        // Without a ledger nothing explains it (a fresh host).
        assert_eq!(unexplained(&[explained], None), vec![format!("/etc/systemd/system/{}", ledger::quarantine_name(&nonce, 0))]);
        // Every directory a ledgered object sits in is scanned.
        let parents = quarantine_parents();
        for parent in ["/etc/systemd/system", "/var/lib/duduclaw-protected", "/usr/local/sbin", "/opt", "/run", "/etc/duduclaw"] {
            assert!(parents.iter().any(|scanned| scanned == parent), "{parent}");
        }
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
            keep: Vec::new(), quarantine: None };
        // A package or an administrator replaced it after the interruption.
        fs::write(directory.path().join("unit.pkg"), b"theirs").unwrap();
        fs::rename(directory.path().join("unit.pkg"), &path).unwrap();
        assert_eq!(remove_quarantined(&dir,c"unit", &entry, &boot_id, me()).unwrap(), Removal::Kept(UNIT_PATH.into()));
        assert_eq!(fs::read(&path).unwrap(), b"theirs");
        assert!(finish(&[entry.path.clone()], &[]).is_err(), "an incomplete rollback never reports done");
        // An intent without an identity matches nothing either.
        let intent = Entry { identity: None, state: State::Intent, ..entry.clone() };
        assert!(matches!(remove_quarantined(&dir,c"unit", &intent, &boot_id, me()).unwrap(), Removal::Kept(_)));
        assert!(path.exists());
        // The recorded object itself is removed; then the path is simply absent.
        let ours = Entry { identity: ledger::identity_at(&dir, c"unit", Kind::File).unwrap(), ..entry.clone() };
        assert_eq!(remove_quarantined(&dir,c"unit", &ours, &boot_id, me()).unwrap(), Removal::Removed);
        assert!(!path.exists());
        assert!(names(directory.path()).is_empty(), "no quarantine is left behind");
        assert_eq!(remove_quarantined(&dir,c"unit", &ours, &boot_id, me()).unwrap(), Removal::Absent);
        // A hard-linked object is kept even when it is the recorded inode (moved back from the quarantine).
        fs::write(&path, b"ours").unwrap();
        let linked = Entry { identity: ledger::identity_at(&dir, c"unit", Kind::File).unwrap(), ..entry };
        fs::hard_link(&path, directory.path().join("alias")).unwrap();
        assert!(matches!(remove_quarantined(&dir,c"unit", &linked, &boot_id, me()).unwrap(), Removal::Kept(_)));
        assert_eq!(names(directory.path()), vec!["alias".to_owned(), "unit".to_owned()]);
        finish(&[], &[]).unwrap();
        assert!(finish(&[], &["/usr/lib/duduclaw"]).is_err());
    }

    #[test]
    fn quarantine_detects_swap_after_first_check_and_moves_it_back() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("unit");
        fs::write(&path, b"ours").unwrap();
        let dir = fs::File::open(directory.path()).unwrap();
        let boot_id = ledger::boot_id().unwrap();
        let entry = recorded_entry(&dir, c"unit", Kind::File);
        // A concurrent rename swaps another object in right after the first check passed.
        let swap = directory.path().to_path_buf();
        AFTER_FIRST_CHECK.with(|hook| *hook.borrow_mut() = Some(Box::new(move || {
            fs::write(swap.join("unit.other"), b"theirs").unwrap();
            fs::rename(swap.join("unit.other"), swap.join("unit")).unwrap();
        })));
        let result = remove_quarantined(&dir,c"unit", &entry, &boot_id, me());
        AFTER_FIRST_CHECK.with(|hook| *hook.borrow_mut() = None);
        assert_eq!(result.unwrap(), Removal::Kept(UNIT_PATH.into()), "detected inside the quarantine");
        // Kept, moved back to its own name, and the empty quarantine is gone.
        assert_eq!(fs::read(&path).unwrap(), b"theirs");
        assert_eq!(names(directory.path()), vec!["unit".to_owned()]);
        assert!(finish(&[UNIT_PATH.into()], &[]).is_err());
        // The same for a directory swapped for another directory (a freed
        // inode number may be reused at once, so the impostor's mode differs too).
        use std::os::unix::fs::PermissionsExt;
        fs::create_dir(directory.path().join("dir")).unwrap();
        fs::set_permissions(directory.path().join("dir"), fs::Permissions::from_mode(0o755)).unwrap();
        let entry = recorded_entry(&dir, c"dir", Kind::Directory);
        let swap = directory.path().to_path_buf();
        AFTER_FIRST_CHECK.with(|hook| *hook.borrow_mut() = Some(Box::new(move || {
            fs::remove_dir(swap.join("dir")).unwrap();
            fs::create_dir(swap.join("dir")).unwrap();
            fs::set_permissions(swap.join("dir"), fs::Permissions::from_mode(0o700)).unwrap();
        })));
        let result = remove_quarantined(&dir,c"dir", &entry, &boot_id, me());
        AFTER_FIRST_CHECK.with(|hook| *hook.borrow_mut() = None);
        assert!(matches!(result.unwrap(), Removal::Kept(_)));
        assert!(directory.path().join("dir").is_dir());
        assert_eq!(names(directory.path()), vec!["dir".to_owned(), "unit".to_owned()]);
        // Unchanged: a tree root is removed through the quarantine with its contents.
        fs::create_dir_all(directory.path().join("tree/a")).unwrap();
        fs::write(directory.path().join("tree/a/file"), b"x").unwrap();
        let tree = Entry { path: "/usr/lib/duduclaw".into(), kind: Kind::Tree, state: State::Created,
            identity: ledger::identity_at(&dir, c"tree", Kind::Tree).unwrap(), keep: Vec::new(), quarantine: None };
        assert_eq!(remove_quarantined(&dir,c"tree", &tree, &boot_id, me()).unwrap(), Removal::Removed);
        assert_eq!(names(directory.path()), vec!["dir".to_owned(), "unit".to_owned()]);
    }

    #[test]
    fn quarantine_refuses_parent_not_owned_or_writable_by_others() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("unit");
        fs::write(&path, b"ours").unwrap();
        let dir = fs::File::open(directory.path()).unwrap();
        let boot_id = ledger::boot_id().unwrap();
        let entry = recorded_entry(&dir, c"unit", Kind::File);
        // The parent is not owned by the required owner (production: root).
        assert!(remove_quarantined(&dir,c"unit", &entry, &boot_id, me().wrapping_add(1)).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"ours");
        assert_eq!(names(directory.path()), vec!["unit".to_owned()]);
        // Writable by others.
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o777)).unwrap();
        assert!(remove_quarantined(&dir,c"unit", &entry, &boot_id, me()).is_err());
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(names(directory.path()), vec!["unit".to_owned()]);
        assert_eq!(remove_quarantined(&dir,c"unit", &entry, &boot_id, me()).unwrap(), Removal::Removed);
    }

    #[test]
    fn post_reboot_run_entries_are_left_to_the_system_and_reported() {
        let boot = "0123abcd-0000-4000-8000-00000000beef";
        let identity = Identity { device: 1, inode: 2, mount_id: 3, mode: libc::S_IFDIR | 0o755, uid: 0, gid: 0,
            size: None, sha256: None, boot_id: "ffffffff-0000-4000-8000-000000000000".into() };
        let runtime = Entry { path: boot::SPAWND_RUNTIME.into(), kind: Kind::Tree, state: State::Created,
            identity: Some(identity.clone()), keep: Vec::new(), quarantine: None };
        // Recorded in another boot: never deleted by rollback.
        assert!(from_earlier_boot(&runtime, boot));
        // Same boot: the ordinary identity rule applies.
        let same_boot = Entry { identity: Some(Identity { boot_id: boot.into(), ..identity.clone() }), ..runtime.clone() };
        assert!(!from_earlier_boot(&same_boot, boot));
        // Not under /run, or never confirmed: the ordinary rule (kept unless it matches).
        let durable = Entry { path: "/var/lib/duduclaw-spawnd".into(), ..runtime.clone() };
        assert!(!from_earlier_boot(&durable, boot));
        let unconfirmed = Entry { identity: None, state: State::Intent, ..runtime.clone() };
        assert!(!from_earlier_boot(&unconfirmed, boot));
        // Its owning units are known; an unknown runtime path has none (kept).
        assert_eq!(runtime_owners(boot::SPAWND_RUNTIME), Some(&["duduclaw-spawnd.socket", "duduclaw-spawnd.service"][..]));
        assert_eq!(runtime_owners("/run/other"), None);
        // Facts below a left runtime directory do not make the rollback incomplete.
        let below_runtime: Vec<&str> = duduclaw_core::isolation_install::FACTS.iter().copied()
            .filter(|fact| fact.starts_with(&format!("{}/", boot::SPAWND_RUNTIME))).collect();
        let everything = remaining_facts(|_| true, &[]);
        let left = remaining_facts(|_| true, &[boot::SPAWND_RUNTIME.to_owned()]);
        for fact in &below_runtime { assert!(everything.contains(fact) && !left.contains(fact), "{fact}"); }
        assert_eq!(left.len() + below_runtime.len(), everything.len());
        report_left(&[boot::SPAWND_RUNTIME.to_owned()]);
        finish(&[], &remaining_facts(|_| false, &[boot::SPAWND_RUNTIME.to_owned()])).unwrap();
    }
}
