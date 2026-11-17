//! v1.83 isolation bootstrap: the durable CREATION LEDGER (review F4).
//!
//! `/var/lib/duduclaw-protected/isolation-creation-ledger.json` (root 0600,
//! strict `deny_unknown_fields`, bounded), written with the journal's
//! discipline: temp → fsync → rename → directory fsync through the verified
//! dirfd of the protected root, then read back. It exists from S1 until the
//! commit (or the end of a rollback) and belongs to one generation.
//!
//! Lifecycle of every object the install transaction creates or replaces:
//!   1. BEFORE creating it: an `intent` entry (path, kind) is made durable. A
//!      file or tree already present at a path that has no entry was not
//!      created by this transaction: the step refuses instead of replacing or
//!      adopting it. A pre-existing directory without an entry is used as it
//!      is and never ledgered (rollback never touches it).
//!   2. AFTER creating it: its identity is made durable (`created`): kind,
//!      device, inode, mount id, mode, owner, the boot it was observed in, and
//!      for regular files size and SHA-256.
//! Rollback deletes ONLY entries whose current identity equals the recorded
//! one ([`same_object`]); anything else at a ledgered path is kept and
//! reported, and the rollback ends incomplete (resumable). An entry still in
//! `intent` without an identity matches nothing. Paths are a fixed allowlist
//! ([`permitted`]), so a damaged or forged ledger cannot aim rollback elsewhere.
//!
//! Review H3: re-intending an entry (a resumed or re-run step) requires the
//! object present now to be exactly the recorded one (same rule as rollback),
//! or no object at all; an intent without an identity admits only an absent
//! object (never adopted). Otherwise the step refuses by path and the entry is
//! left untouched. The ledger carries its transaction's nonce (the journal's)
//! and is refused for any other transaction, even of the same generation.
use super::*;
use super::bootstrap::{self as boot, Store};
use std::{ffi::{CStr, CString}, io::Read, os::{fd::RawFd, unix::ffi::OsStrExt}};
use serde::{Deserialize, Serialize};

pub(super) const LEDGER_FILE: &str = "isolation-creation-ledger.json";
const MAX_LEDGER_BYTES: usize = 256 * 1024;
const MAX_ENTRIES: usize = 256;
const MAX_KEEP: usize = 64;
/// Bound of one hashed regular file (the largest program artifact is far below).
const MAX_HASHED_BYTES: u64 = 4 * 1024 * 1024 * 1024;
/// The gateway's private data directory (`Kind::Contents`).
pub(super) const GATEWAY_DATA_PATH: &str = "/var/lib/duduclaw/.duduclaw";
pub(super) const LEDGER_DAMAGED: &str = "隔離安裝的建立紀錄無法驗證（可能已損毀或被修改），已停止。請勿刪除它；請保留現況並聯絡原廠協助人工處理。";
pub(super) const MOUNT_ID_UNAVAILABLE: &str = "此核心無法回報掛載識別碼（需要 Linux 5.8 以上的 statx STATX_MNT_ID），無法安全判斷要移除的物件，已停止。";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Kind {
    /// A regular file: unit, program, launcher, policy file, version file, root key.
    File,
    /// A directory; removed only when empty after its ledgered children are gone.
    Directory,
    /// A directory created by this transaction whose contents are written by
    /// the components it runs (R1's publish, the parse-tree builder, systemd,
    /// spawnd): removed recursively, never crossing a mount, never following a link.
    Tree,
    /// The gateway's private data directory: the directory and the names
    /// recorded in `keep` (present before the gateway first started, at S8)
    /// stay; only what appeared after that snapshot is removed.
    Contents,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum State { Intent, Created }

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Identity {
    pub(super) device: u64,
    pub(super) inode: u64,
    pub(super) mount_id: u64,
    /// `st_mode` including the file type bits.
    pub(super) mode: u32,
    pub(super) uid: u32,
    pub(super) gid: u32,
    pub(super) size: Option<u64>,
    pub(super) sha256: Option<String>,
    /// `/proc/sys/kernel/random/boot_id` when observed: mount ids are per boot.
    pub(super) boot_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Entry {
    pub(super) path: String,
    pub(super) kind: Kind,
    pub(super) state: State,
    /// The last confirmed identity (kept while a re-creation is `intent`).
    pub(super) identity: Option<Identity>,
    pub(super) keep: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Ledger {
    pub(super) schema_version: u16,
    pub(super) generation: String,
    /// The journal's transaction nonce (review H3).
    pub(super) nonce: String,
    pub(super) entries: Vec<Entry>,
}

/// Review H3: the ledger belongs to exactly this transaction.
pub(super) fn belongs_to(ledger: &Ledger, generation: &str, nonce: &str) -> Result<()> {
    if ledger.generation != generation || ledger.nonce != nonce { return Err(refused(LEDGER_DAMAGED)); }
    Ok(())
}

fn boot_id_valid(text: &str) -> bool {
    text.len() == 36 && text.bytes().all(|b| b == b'-' || b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn single_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 255 && name != "." && name != ".." && !name.contains('/') && !name.contains('\0')
}

/// The fixed paths rollback may act on, by kind.
pub(super) fn permitted(kind: Kind, path: &str) -> bool {
    match kind {
        Kind::File => super::rollback::ledger_files().iter().any(|file| file == path),
        Kind::Directory => super::rollback::LEDGER_DIRECTORIES.contains(&path),
        Kind::Tree => super::rollback::TREES.contains(&path),
        Kind::Contents => path == GATEWAY_DATA_PATH,
    }
}

impl Identity {
    fn valid_for(&self, kind: Kind) -> bool {
        let file_type = self.mode & libc::S_IFMT;
        self.mount_id != 0 && boot_id_valid(&self.boot_id) && match kind {
            Kind::File => file_type == libc::S_IFREG && self.size.is_some()
                && self.sha256.as_deref().is_some_and(|digest| boot::parse_generation(digest).is_some()),
            Kind::Directory | Kind::Tree | Kind::Contents =>
                file_type == libc::S_IFDIR && self.size.is_none() && self.sha256.is_none(),
        }
    }
}

impl Ledger {
    pub(super) fn new(generation: &str, nonce: &str) -> Self {
        Self { schema_version: 2, generation: generation.to_owned(), nonce: nonce.to_owned(), entries: Vec::new() }
    }

    pub(super) fn validate(&self) -> Result<()> {
        let damaged = || refused(LEDGER_DAMAGED);
        if self.schema_version != 2 || boot::parse_generation(&self.generation).is_none()
            || boot::parse_generation(&self.nonce).is_none() || self.entries.len() > MAX_ENTRIES {
            return Err(damaged());
        }
        for (index, entry) in self.entries.iter().enumerate() {
            if self.entries.iter().take(index).any(|other| other.path == entry.path) || !permitted(entry.kind, &entry.path)
                || (entry.state == State::Created && entry.identity.is_none())
                || entry.identity.as_ref().is_some_and(|identity| !identity.valid_for(entry.kind))
                || (entry.kind != Kind::Contents && !entry.keep.is_empty()) || entry.keep.len() > MAX_KEEP
                || !entry.keep.iter().all(|name| single_name(name)) {
                return Err(damaged());
            }
        }
        Ok(())
    }

    pub(super) fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > MAX_LEDGER_BYTES { return Err(refused(LEDGER_DAMAGED)); }
        let ledger: Self = serde_json::from_slice(bytes).map_err(|_| refused(LEDGER_DAMAGED))?;
        ledger.validate()?;
        Ok(ledger)
    }

    pub(super) fn entry(&self, path: &str) -> Option<&Entry> { self.entries.iter().find(|entry| entry.path == path) }

    /// `path` is a ledgered object or lies below a ledgered tree (S1 on resume, review F6).
    pub(super) fn covers(&self, path: &str) -> bool {
        self.entries.iter().any(|entry| entry.path == path
            || (entry.kind == Kind::Tree && path.strip_prefix(entry.path.as_str()).is_some_and(|rest| rest.starts_with('/'))))
    }
}

// ── Durable storage in the protected root ─────────────────────────────────

fn missing(error: &duduclaw_updated::Error) -> bool {
    matches!(error, duduclaw_updated::Error::Io(error) if error.kind() == std::io::ErrorKind::NotFound)
}

/// NotFound = `None`; anything that is not a strict root 0600 ledger refuses.
pub(super) fn read(store: &Store) -> Result<Option<Ledger>> {
    let file = match store.directory().read(LEDGER_FILE) {
        Ok(file) => file,
        Err(error) if missing(&error) => return Ok(None),
        Err(_) => return Err(refused(LEDGER_DAMAGED)),
    };
    let metadata = file.metadata()?;
    if metadata.mode() & 0o7777 != 0o600 || metadata.len() > MAX_LEDGER_BYTES as u64 { return Err(refused(LEDGER_DAMAGED)); }
    let mut bytes = Vec::new();
    file.take(MAX_LEDGER_BYTES as u64 + 1).read_to_end(&mut bytes)?;
    Ledger::parse(&bytes).map(Some)
}

/// Durable, checked write; the result is read back.
pub(super) fn write(store: &Store, ledger: &Ledger) -> Result<()> {
    ledger.validate()?;
    if serde_json::to_vec(ledger).map_err(|_| refused(LEDGER_DAMAGED))?.len() > MAX_LEDGER_BYTES {
        return Err(refused(LEDGER_DAMAGED));
    }
    store.directory().atomic_json(LEDGER_FILE, ledger).map_err(|_| refused("無法寫入隔離安裝的建立紀錄。"))?;
    if read(store)?.as_ref() != Some(ledger) { return Err(refused(LEDGER_DAMAGED)); }
    Ok(())
}

pub(super) fn remove(store: &Store) -> Result<()> {
    match store.directory().remove(LEDGER_FILE) {
        Ok(()) => Ok(()),
        Err(error) if missing(&error) => Ok(()),
        Err(_) => Err(refused("無法移除隔離安裝的建立紀錄。")),
    }
}

// ── Identity of an object (statx; test seam for the mount id) ─────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Stat {
    /// `major << 32 | minor`.
    pub(super) device: u64,
    pub(super) inode: u64,
    pub(super) mode: u32,
    pub(super) uid: u32,
    pub(super) gid: u32,
    pub(super) size: u64,
    pub(super) nlink: u64,
    /// `None` when the kernel does not report `STATX_MNT_ID` (before 5.8).
    pub(super) mount_id: Option<u64>,
}

#[cfg(test)]
type MountSeam = Box<dyn Fn(&CStr, Option<u64>) -> Option<u64>>;
#[cfg(test)]
thread_local! {
    /// Unit-test seam: rewrites the kernel's mount id answer for an entry name
    /// (the empty name is the directory itself). Never set in production.
    pub(super) static MOUNT_ID_SEAM: std::cell::RefCell<Option<MountSeam>> = const { std::cell::RefCell::new(None) };
}

/// `statx` of `name` below `dirfd` (empty `name`: `dirfd` itself) without
/// following a final link and without triggering an automount.
pub(super) fn stat_at(dirfd: RawFd, name: &CStr) -> std::io::Result<Stat> {
    let flags = libc::AT_SYMLINK_NOFOLLOW | libc::AT_NO_AUTOMOUNT | if name.is_empty() { libc::AT_EMPTY_PATH } else { 0 };
    // SAFETY: an all-zero `statx` (plain integers) is a valid value.
    let mut stat: libc::statx = unsafe { std::mem::zeroed() };
    duduclaw_core::linux_syscalls::statx(dirfd, name, flags, libc::STATX_BASIC_STATS | libc::STATX_MNT_ID, &mut stat)?;
    let required = libc::STATX_TYPE | libc::STATX_MODE | libc::STATX_UID | libc::STATX_GID | libc::STATX_INO
        | libc::STATX_NLINK | libc::STATX_SIZE;
    if stat.stx_mask & required != required { return Err(std::io::Error::other("incomplete statx answer")); }
    let mount_id = (stat.stx_mask & libc::STATX_MNT_ID != 0 && stat.stx_mnt_id != 0).then_some(stat.stx_mnt_id);
    #[cfg(test)]
    let mount_id = MOUNT_ID_SEAM.with(|seam| match seam.borrow().as_ref() { Some(rewrite) => rewrite(name, mount_id), None => mount_id });
    Ok(Stat { device: (u64::from(stat.stx_dev_major) << 32) | u64::from(stat.stx_dev_minor), inode: stat.stx_ino,
        mode: u32::from(stat.stx_mode), uid: stat.stx_uid, gid: stat.stx_gid, size: stat.stx_size,
        nlink: u64::from(stat.stx_nlink), mount_id })
}

pub(super) fn boot_id() -> Result<String> {
    let text = fs::read_to_string("/proc/sys/kernel/random/boot_id")?;
    let text = text.trim();
    if !boot_id_valid(text) { return Err(refused("無法讀取開機識別碼。")); }
    Ok(text.to_owned())
}

/// A fixed absolute path's parent (every ancestor root-owned, not writable by
/// others, never a link) opened no-follow, and the final name.
pub(super) fn open_parent(path: &str) -> Result<(fs::File, CString)> {
    let path = Path::new(path);
    parents(path)?;
    let parent = path.parent().ok_or_else(|| refused("無效的路徑。"))?;
    let name = CString::new(path.file_name().ok_or_else(|| refused("無效的路徑。"))?.as_bytes())
        .map_err(|_| refused("無效的路徑。"))?;
    let directory = fs::OpenOptions::new().read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC).open(parent)?;
    Ok((directory, name))
}

/// SHA-256 of the regular file `name`, opened no-follow and proven to be the stat'ed inode.
fn hash_at(directory: &fs::File, name: &CStr, stat: &Stat) -> Result<String> {
    use sha2::{Digest, Sha256};
    use std::os::fd::FromRawFd;
    // SAFETY: live dirfd and a NUL-terminated single-component name.
    let fd = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(),
        libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC) };
    if fd < 0 { return Err(std::io::Error::last_os_error().into()); }
    // SAFETY: a fresh descriptor owned here.
    let file = unsafe { fs::File::from_raw_fd(fd) };
    let opened = stat_at(file.as_raw_fd(), c"")?;
    if opened.inode != stat.inode || opened.device != stat.device || opened.size != stat.size || stat.size > MAX_HASHED_BYTES {
        return Err(refused("要記錄的檔案在檢查期間被更換，已停止。"));
    }
    let mut hash = Sha256::new();
    let mut total = 0u64;
    let mut buffer = vec![0u8; 1024 * 1024];
    let mut file = file.take(stat.size + 1);
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 { break; }
        total = total.saturating_add(count as u64);
        hash.update(buffer.get(..count).ok_or_else(|| refused("讀取失敗。"))?);
    }
    if total != stat.size { return Err(refused("要記錄的檔案在檢查期間被修改，已停止。")); }
    Ok(hex::encode(hash.finalize()))
}

/// The identity of `name` below `directory` as `kind`; `None` when absent.
/// A kernel without `STATX_MNT_ID` refuses (never a device-only fallback).
pub(super) fn identity_at(directory: &fs::File, name: &CStr, kind: Kind) -> Result<Option<Identity>> {
    let stat = match stat_at(directory.as_raw_fd(), name) {
        Ok(stat) => stat,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let mount_id = stat.mount_id.ok_or_else(|| refused(MOUNT_ID_UNAVAILABLE))?;
    let (size, sha256) = if kind == Kind::File && stat.mode & libc::S_IFMT == libc::S_IFREG {
        (Some(stat.size), Some(hash_at(directory, name, &stat)?))
    } else { (None, None) };
    Ok(Some(Identity { device: stat.device, inode: stat.inode, mount_id, mode: stat.mode, uid: stat.uid, gid: stat.gid,
        size, sha256, boot_id: boot_id()? }))
}

/// The rollback rule (pure). Same inode, device, owner and full mode (type and
/// permission bits, directories included: review L1 — the installer's own
/// later mode re-application is re-recorded through [`Recorder::rerecord_mode`]);
/// for regular files also size and SHA-256. Mount ids are per boot: in the
/// recording boot they must be equal; after a reboot the object must sit on
/// its parent directory's mount (it is not a mount point of its own).
pub(super) fn same_object(recorded: &Identity, current: &Identity, parent_mount: u64, boot: &str) -> bool {
    let mount = if recorded.boot_id == boot { current.mount_id == recorded.mount_id } else { current.mount_id == parent_mount };
    mount && recorded.mode == current.mode && recorded.device == current.device && recorded.inode == current.inode
        && recorded.uid == current.uid && recorded.gid == current.gid && recorded.size == current.size
        && recorded.sha256 == current.sha256
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Now { Absent, Recorded, Other }

/// What `name` below `directory` holds now, relative to `entry`.
pub(super) fn classify_at(directory: &fs::File, name: &CStr, entry: &Entry, boot: &str) -> Result<Now> {
    let Some(current) = identity_at(directory, name, entry.kind)? else { return Ok(Now::Absent) };
    let parent = stat_at(directory.as_raw_fd(), c"")?.mount_id.ok_or_else(|| refused(MOUNT_ID_UNAVAILABLE))?;
    Ok(match &entry.identity {
        Some(recorded) if same_object(recorded, &current, parent, boot) => Now::Recorded,
        _ => Now::Other,
    })
}

pub(super) fn present(path: &str) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

// ── The install transaction's recorder ────────────────────────────────────

/// Where the recorder persists and observes (test seam).
pub(super) trait LedgerHost {
    /// Durable write of the whole ledger (read back before returning).
    fn save(&mut self, ledger: &Ledger) -> Result<()>;
    fn present(&mut self, path: &str) -> Result<bool>;
    fn identity(&mut self, path: &str, kind: Kind) -> Result<Option<Identity>>;
    /// What the entry's path holds now relative to the entry (rollback's rule).
    fn classify(&mut self, entry: &Entry) -> Result<Now>;
}

pub(super) struct LiveHost { store: Store }

impl LedgerHost for LiveHost {
    fn save(&mut self, ledger: &Ledger) -> Result<()> { write(&self.store, ledger) }
    fn present(&mut self, path: &str) -> Result<bool> { present(path) }
    fn identity(&mut self, path: &str, kind: Kind) -> Result<Option<Identity>> {
        if !present(path)? { return Ok(None); }
        let (directory, name) = open_parent(path)?;
        identity_at(&directory, &name, kind)
    }
    fn classify(&mut self, entry: &Entry) -> Result<Now> {
        if !present(&entry.path)? { return Ok(Now::Absent); }
        let (directory, name) = open_parent(&entry.path)?;
        classify_at(&directory, &name, entry, &boot_id()?)
    }
}

pub(super) struct Recorder<H: LedgerHost> { host: H, ledger: Ledger }
pub(super) type LiveRecorder = Recorder<LiveHost>;

impl Recorder<LiveHost> {
    /// The ledger of this transaction (`generation`, `nonce`) in the protected
    /// root; created empty when absent.
    pub(super) fn open_live(generation: &str, nonce: &str) -> Result<Self> {
        let store = Store::open()?;
        let existing = read(&store)?;
        Recorder::open(LiveHost { store }, existing, generation, nonce)
    }
}

/// Review H3: an existing object at a ledgered path the step would create again.
fn changed_since_recorded(path: &str) -> DuDuClawError {
    refused(format!("{path} 上的物件已不是這次安裝記錄的原物件（已被更換或修改）；已停止，未覆蓋、接管或移除它，建立紀錄也未變更。請確認來源後再續接，或執行 duduclaw-bootstrap rollback。"))
}

fn never_adopted(path: &str) -> DuDuClawError {
    refused(format!("{path} 已存在，但這次安裝在建立它之前中斷，沒有記錄它的識別；無法證明它是這次安裝建立的，已停止，未覆蓋或接管它。請確認來源並處理後再續接，或執行 duduclaw-bootstrap rollback。"))
}

impl<H: LedgerHost> Recorder<H> {
    pub(super) fn open(mut host: H, existing: Option<Ledger>, generation: &str, nonce: &str) -> Result<Self> {
        let ledger = match existing {
            Some(ledger) => { belongs_to(&ledger, generation, nonce)?; ledger }
            None => {
                let ledger = Ledger::new(generation, nonce);
                host.save(&ledger)?;
                ledger
            }
        };
        Ok(Self { host, ledger })
    }

    pub(super) fn ledger(&self) -> &Ledger { &self.ledger }

    fn save(&mut self, next: Ledger) -> Result<()> {
        if next != self.ledger {
            self.host.save(&next)?;
            self.ledger = next;
        }
        Ok(())
    }

    /// Step 1 of the lifecycle, one durable write for all `items`. Review H3:
    /// an existing entry is re-intended only when its path holds nothing or
    /// exactly the recorded object; nothing is written when any item refuses.
    pub(super) fn intend(&mut self, items: &[(&str, Kind)]) -> Result<()> {
        let mut next = self.ledger.clone();
        for (path, kind) in items {
            if !permitted(*kind, path) { return Err(refused(LEDGER_DAMAGED)); }
            match next.entries.iter_mut().find(|entry| entry.path == *path) {
                Some(entry) if entry.kind == *kind => {
                    match (entry.identity.is_some(), self.host.classify(entry)?) {
                        (_, Now::Absent) | (true, Now::Recorded) => (),
                        (true, Now::Other) => return Err(changed_since_recorded(path)),
                        (false, _) => return Err(never_adopted(path)),
                    }
                    entry.state = State::Intent;
                }
                Some(_) => return Err(refused(LEDGER_DAMAGED)),
                None if self.host.present(path)? => match kind {
                    // Not created by this transaction: used as it is, never ledgered.
                    Kind::Directory => (),
                    _ => return Err(refused(format!("{path} 已存在，但不是這次安裝建立的；已停止，未覆蓋或移除它。請確認來源後再續接，或執行 duduclaw-bootstrap rollback。"))),
                },
                None => next.entries.push(Entry { path: (*path).to_owned(), kind: *kind, state: State::Intent,
                    identity: None, keep: Vec::new() }),
            }
        }
        self.save(next)
    }

    /// Step 2 of the lifecycle: the identity of every listed path that has an
    /// entry and now exists, one durable write. An absent object keeps its intent.
    pub(super) fn confirm(&mut self, paths: &[&str]) -> Result<()> {
        let mut next = self.ledger.clone();
        for path in paths {
            let Some(entry) = next.entries.iter_mut().find(|entry| entry.path == *path) else { continue };
            if let Some(identity) = self.host.identity(path, entry.kind)? {
                if !identity.valid_for(entry.kind) { return Err(refused(format!("建立後讀回的物件型別不符：{path}"))); }
                entry.identity = Some(identity);
                entry.state = State::Created;
            }
        }
        self.save(next)
    }

    /// Intent, create, identity. The identities are recorded even when the
    /// creation failed part-way: the intent proved each path empty (or the
    /// recorded object) right before, so what exists now is this
    /// transaction's, and a resume then never meets an identity-less object
    /// it would have to refuse (review H3).
    pub(super) fn record<T>(&mut self, items: &[(&str, Kind)], create: impl FnOnce() -> Result<T>) -> Result<T> {
        self.intend(items)?;
        let value = create();
        let paths: Vec<&str> = items.iter().map(|(path, _)| *path).collect();
        let confirmed = self.confirm(&paths);
        let value = value?;
        confirmed?;
        Ok(value)
    }

    /// Review L1: the installer re-applied the permission bits `mode` to a
    /// ledgered directory it created. When the object present is exactly the
    /// recorded one with only those bits changed to `mode`, that identity is
    /// made durable; any other state is left for the intent and rollback rules.
    pub(super) fn rerecord_mode(&mut self, path: &str, mode: u32) -> Result<()> {
        if mode & !0o7777 != 0 { return Err(refused(LEDGER_DAMAGED)); }
        let Some(entry) = self.ledger.entry(path) else { return Ok(()) };
        if entry.kind == Kind::File { return Err(refused(LEDGER_DAMAGED)); }
        let Some(recorded) = &entry.identity else { return Ok(()) };
        if recorded.mode & 0o7777 == mode { return Ok(()); }
        let mut transitioned = entry.clone();
        if let Some(identity) = transitioned.identity.as_mut() { identity.mode = (identity.mode & libc::S_IFMT) | mode; }
        if self.host.classify(&transitioned)? != Now::Recorded { return Ok(()); }
        let mut next = self.ledger.clone();
        for entry in next.entries.iter_mut().filter(|entry| entry.path == path) { *entry = transitioned.clone(); }
        self.save(next)
    }

    /// The gateway data snapshot (S8, before the gateway can first start). The
    /// first snapshot stands for the life of the transaction.
    pub(super) fn contents(&mut self, path: &str, keep: Vec<String>) -> Result<()> {
        if self.ledger.entry(path).is_some() { return Ok(()); }
        let identity = self.host.identity(path, Kind::Contents)?.ok_or_else(|| refused("Gateway 資料目錄不存在。"))?;
        let mut next = self.ledger.clone();
        next.entries.push(Entry { path: path.to_owned(), kind: Kind::Contents, state: State::Created,
            identity: Some(identity), keep });
        self.save(next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, rc::Rc};

    fn identity(kind: Kind, inode: u64) -> Identity {
        let file = kind == Kind::File;
        Identity { device: 8 << 32 | 1, inode, mount_id: 31, mode: if file { libc::S_IFREG | 0o644 } else { libc::S_IFDIR | 0o755 },
            uid: 0, gid: 0, size: file.then_some(10), sha256: file.then(|| "ab".repeat(32)),
            boot_id: "0123abcd-0000-4000-8000-00000000beef".into() }
    }

    fn nonce() -> String { "5a".repeat(32) }

    fn ledger() -> Ledger {
        let mut ledger = Ledger::new(&"cd".repeat(32), &nonce());
        ledger.entries.push(Entry { path: UNIT_PATH.into(), kind: Kind::File, state: State::Created,
            identity: Some(identity(Kind::File, 7)), keep: Vec::new() });
        ledger.entries.push(Entry { path: "/opt/duduclaw-isolated".into(), kind: Kind::Tree, state: State::Intent,
            identity: None, keep: Vec::new() });
        ledger.entries.push(Entry { path: GATEWAY_DATA_PATH.into(), kind: Kind::Contents, state: State::Created,
            identity: Some(identity(Kind::Contents, 9)), keep: vec!["license.json".into()] });
        ledger
    }

    #[test]
    fn ledger_strict_parse_and_fixed_paths() {
        let good = ledger();
        let bytes = serde_json::to_vec(&good).unwrap();
        assert_eq!(Ledger::parse(&bytes).unwrap(), good);
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let mutate = |change: fn(&mut serde_json::Value)| {
            let mut value = value.clone();
            change(&mut value);
            serde_json::to_vec(&value).unwrap()
        };
        for bad in [mutate(|v| v["extra"] = true.into()), mutate(|v| v["schema_version"] = 1.into()),
            mutate(|v| v["schema_version"] = 3.into()),
            mutate(|v| v["generation"] = "CD".repeat(32).into()),
            mutate(|v| v["nonce"] = "5A".repeat(32).into()), mutate(|v| v["nonce"] = "5a".into()),
            mutate(|v| { v.as_object_mut().unwrap().remove("nonce"); }),
            mutate(|v| v["entries"][0]["extra"] = 1.into()),
            mutate(|v| v["entries"][0]["identity"]["extra"] = 1.into()),
            // Not an installer path: a forged ledger cannot aim rollback elsewhere.
            mutate(|v| v["entries"][0]["path"] = "/etc/passwd".into()),
            mutate(|v| v["entries"][1]["path"] = "/var/lib".into()),
            mutate(|v| v["entries"][1]["kind"] = "file".into()),
            mutate(|v| v["entries"][1]["path"] = UNIT_PATH.into()),
            mutate(|v| v["entries"][0]["identity"] = serde_json::Value::Null),
            mutate(|v| v["entries"][0]["identity"]["sha256"] = serde_json::Value::Null),
            mutate(|v| v["entries"][0]["identity"]["mount_id"] = 0.into()),
            mutate(|v| v["entries"][0]["identity"]["boot_id"] = "x".into()),
            mutate(|v| v["entries"][0]["keep"] = serde_json::json!(["a"])),
            mutate(|v| v["entries"][2]["keep"] = serde_json::json!(["../x"])),
            mutate(|v| v["entries"][0]["state"] = "deleted".into()),
            b"[]".to_vec(), Vec::new()] {
            assert!(Ledger::parse(&bad).is_err(), "{}", String::from_utf8_lossy(&bad));
        }
        assert!(Ledger::parse(&vec![b' '; MAX_LEDGER_BYTES + 1]).is_err());
        let mut many = Ledger::new(&"cd".repeat(32), &nonce());
        for _ in 0..=MAX_ENTRIES {
            many.entries.push(Entry { path: UNIT_PATH.into(), kind: Kind::File, state: State::Intent, identity: None, keep: Vec::new() });
        }
        assert!(many.validate().is_err());
        assert!(good.covers("/opt/duduclaw-isolated/current") && good.covers(UNIT_PATH));
        assert!(!good.covers("/opt/duduclaw-isolated-other") && !good.covers(BINARY));
    }

    #[test]
    fn ledger_identity_matching_rules() {
        let boot = "0123abcd-0000-4000-8000-00000000beef";
        let recorded = identity(Kind::File, 7);
        assert!(same_object(&recorded, &recorded, 31, boot));
        let changes: [fn(&mut Identity); 7] = [|i| i.inode += 1, |i| i.device += 1, |i| i.mount_id += 1,
            |i| i.mode = libc::S_IFREG | 0o600, |i| i.uid = 1, |i| i.size = Some(11), |i| i.sha256 = Some("cd".repeat(32))];
        for change in changes {
            let mut current = recorded.clone();
            change(&mut current);
            assert!(!same_object(&recorded, &current, 31, boot), "{current:?}");
        }
        // Another boot: the mount id must be the parent's (not a mount point of its own).
        let mut later = recorded.clone();
        later.mount_id = 77;
        later.boot_id = "ffffffff-0000-4000-8000-000000000000".into();
        assert!(same_object(&recorded, &later, 77, &later.boot_id));
        assert!(!same_object(&recorded, &later, 78, &later.boot_id));
        // Review L1: a directory's permission bits are part of its identity too
        // (the installer's own re-application is re-recorded, see below).
        let directory = identity(Kind::Directory, 5);
        let mut chmod = directory.clone();
        chmod.mode = libc::S_IFDIR | 0o700;
        assert!(!same_object(&directory, &chmod, 31, boot));
        chmod.mode = libc::S_IFREG | 0o755;
        assert!(!same_object(&directory, &chmod, 31, boot));
        assert!(same_object(&directory, &directory.clone(), 31, boot));
    }

    /// In-memory host: objects by path, and every durable ledger write in order.
    #[derive(Default)]
    struct World { objects: Vec<(String, Identity)>, events: Vec<String>, saved: Vec<Ledger> }
    struct FakeHost(Rc<RefCell<World>>);
    impl LedgerHost for FakeHost {
        fn save(&mut self, ledger: &Ledger) -> Result<()> {
            ledger.validate()?;
            let mut world = self.0.borrow_mut();
            let states: Vec<String> = ledger.entries.iter()
                .map(|entry| format!("{}:{:?}:{}", entry.path, entry.state, entry.identity.is_some())).collect();
            world.events.push(format!("save {}", states.join(",")));
            world.saved.push(ledger.clone());
            Ok(())
        }
        fn present(&mut self, path: &str) -> Result<bool> { Ok(self.0.borrow().objects.iter().any(|(p, _)| p == path)) }
        fn identity(&mut self, path: &str, _: Kind) -> Result<Option<Identity>> {
            Ok(self.0.borrow().objects.iter().find(|(p, _)| p == path).map(|(_, identity)| identity.clone()))
        }
        /// Exact identity equality stands in for `same_object` (tested on its own above).
        fn classify(&mut self, entry: &Entry) -> Result<Now> {
            Ok(match self.identity(&entry.path, entry.kind)? {
                None => Now::Absent,
                Some(current) if entry.identity.as_ref() == Some(&current) => Now::Recorded,
                Some(_) => Now::Other,
            })
        }
    }

    impl World {
        fn put(&mut self, path: &str, identity: Identity) {
            self.objects.retain(|(p, _)| p != path);
            self.objects.push((path.into(), identity));
        }
    }

    fn open(world: &Rc<RefCell<World>>) -> Recorder<FakeHost> {
        Recorder::open(FakeHost(world.clone()), None, &"cd".repeat(32), &nonce()).unwrap()
    }

    #[test]
    fn ledger_records_intent_before_create_and_identity_after() {
        let world = Rc::new(RefCell::new(World::default()));
        let mut recorder = open(&world);
        let created = world.clone();
        recorder.record(&[(UNIT_PATH, Kind::File)], move || {
            let mut world = created.borrow_mut();
            world.events.push("create".into());
            world.objects.push((UNIT_PATH.into(), identity(Kind::File, 7)));
            Ok(())
        }).unwrap();
        let events = world.borrow().events.clone();
        assert_eq!(events, vec!["save ".to_owned(), format!("save {UNIT_PATH}:Intent:false"), "create".to_owned(),
            format!("save {UNIT_PATH}:Created:true")]);
        assert_eq!(recorder.ledger().entry(UNIT_PATH).unwrap().identity, Some(identity(Kind::File, 7)));
        // A failed creation leaves only the durable intent (no identity).
        let failed = recorder.record(&[(BINARY, Kind::File)], || -> Result<()> { Err(refused("boom")) });
        assert!(failed.is_err());
        let entry = recorder.ledger().entry(BINARY).unwrap().clone();
        assert_eq!((entry.state, entry.identity), (State::Intent, None));
        // Re-creating keeps the last confirmed identity while in intent.
        recorder.intend(&[(UNIT_PATH, Kind::File)]).unwrap();
        let entry = recorder.ledger().entry(UNIT_PATH).unwrap().clone();
        assert_eq!((entry.state, entry.identity.is_some()), (State::Intent, true));
    }

    #[test]
    fn ledger_refuses_objects_it_did_not_create() {
        let world = Rc::new(RefCell::new(World::default()));
        world.borrow_mut().objects.push((LAUNCHER.into(), identity(Kind::File, 3)));
        world.borrow_mut().objects.push(("/opt/duduclaw".into(), identity(Kind::Directory, 4)));
        world.borrow_mut().objects.push(("/usr/lib/duduclaw".into(), identity(Kind::Tree, 5)));
        let mut recorder = open(&world);
        // A file or tree present without an entry was not created by this transaction.
        assert!(recorder.intend(&[(LAUNCHER, Kind::File)]).is_err());
        assert!(recorder.intend(&[("/usr/lib/duduclaw", Kind::Tree)]).is_err());
        // A pre-existing directory is used and never ledgered.
        recorder.record(&[("/opt/duduclaw", Kind::Directory)], || Ok(())).unwrap();
        assert!(recorder.ledger().entry("/opt/duduclaw").is_none());
        // A path outside the allowlist is never recorded.
        assert!(recorder.intend(&[("/etc/passwd", Kind::File)]).is_err());
        // Another generation's ledger is refused.
        assert!(Recorder::open(FakeHost(world.clone()), Some(Ledger::new(&"ab".repeat(32), &nonce())), &"cd".repeat(32), &nonce()).is_err());
        // Review H3: so is another transaction's ledger of the same generation.
        let other = Ledger::new(&"cd".repeat(32), &"6b".repeat(32));
        assert!(Recorder::open(FakeHost(world.clone()), Some(other.clone()), &"cd".repeat(32), &nonce()).is_err());
        assert!(belongs_to(&other, &"cd".repeat(32), &nonce()).is_err());
        belongs_to(&other, &"cd".repeat(32), &"6b".repeat(32)).unwrap();
        assert!(belongs_to(&other, &"ab".repeat(32), &"6b".repeat(32)).is_err());
        Recorder::open(FakeHost(world), Some(other), &"cd".repeat(32), &"6b".repeat(32)).unwrap();
    }

    #[test]
    fn ledger_reintent_refuses_changed_or_unrecorded_objects() {
        let world = Rc::new(RefCell::new(World::default()));
        let mut recorder = open(&world);
        let created = world.clone();
        recorder.record(&[(UNIT_PATH, Kind::File)], move || {
            created.borrow_mut().put(UNIT_PATH, identity(Kind::File, 7));
            Ok(())
        }).unwrap();
        // The recorded object is still there: re-intent proceeds (resume of the same step).
        recorder.intend(&[(UNIT_PATH, Kind::File)]).unwrap();
        recorder.confirm(&[UNIT_PATH]).unwrap();
        let before = recorder.ledger().clone();
        let saves = world.borrow().saved.len();
        // An administrator or a package replaced it after a crash: refused by path, entry untouched.
        world.borrow_mut().put(UNIT_PATH, identity(Kind::File, 8));
        let error = recorder.intend(&[(UNIT_PATH, Kind::File)]).unwrap_err().to_string();
        assert!(error.contains(UNIT_PATH), "{error}");
        assert_eq!(recorder.ledger(), &before);
        assert_eq!(world.borrow().saved.len(), saves, "nothing written");
        // A refusal of one item writes none of a batch.
        assert!(recorder.intend(&[(BINARY, Kind::File), (UNIT_PATH, Kind::File)]).is_err());
        assert!(recorder.ledger().entry(BINARY).is_none());
        // Missing now: re-creating at an empty path may proceed.
        world.borrow_mut().objects.retain(|(path, _)| path != UNIT_PATH);
        recorder.intend(&[(UNIT_PATH, Kind::File)]).unwrap();
        // ... but reappeared as something else before the re-run: refused again.
        world.borrow_mut().put(UNIT_PATH, identity(Kind::File, 9));
        assert!(recorder.intend(&[(UNIT_PATH, Kind::File)]).is_err());
        // An intent without identity (crash between intent and confirm) never adopts an object.
        let failed = recorder.record(&[(LAUNCHER, Kind::File)], || -> Result<()> { Err(refused("crash")) });
        assert!(failed.is_err());
        assert_eq!(recorder.ledger().entry(LAUNCHER).unwrap().identity, None);
        world.borrow_mut().put(LAUNCHER, identity(Kind::File, 11));
        let before = recorder.ledger().clone();
        let error = recorder.intend(&[(LAUNCHER, Kind::File)]).unwrap_err().to_string();
        assert!(error.contains(LAUNCHER), "{error}");
        assert_eq!(recorder.ledger(), &before);
        // Only an absent object may proceed.
        world.borrow_mut().objects.retain(|(path, _)| path != LAUNCHER);
        recorder.intend(&[(LAUNCHER, Kind::File)]).unwrap();
    }

    #[test]
    fn ledger_records_identity_even_when_creation_failed_part_way() {
        let world = Rc::new(RefCell::new(World::default()));
        let mut recorder = open(&world);
        let created = world.clone();
        let failed = recorder.record(&[("/usr/lib/duduclaw", Kind::Tree)], move || -> Result<()> {
            created.borrow_mut().put("/usr/lib/duduclaw", identity(Kind::Tree, 12));
            Err(refused("failed after creating the directory"))
        });
        assert!(failed.is_err());
        let entry = recorder.ledger().entry("/usr/lib/duduclaw").unwrap().clone();
        assert_eq!((entry.state, entry.identity), (State::Created, Some(identity(Kind::Tree, 12))));
        // So the re-run re-intends it instead of refusing an identity-less object.
        recorder.intend(&[("/usr/lib/duduclaw", Kind::Tree)]).unwrap();
    }

    #[test]
    fn directory_mode_change_differs_unless_it_is_the_installers_own_rerecord() {
        let world = Rc::new(RefCell::new(World::default()));
        let mut recorder = open(&world);
        let created = world.clone();
        recorder.record(&[("/opt/duduclaw", Kind::Directory)], move || {
            created.borrow_mut().put("/opt/duduclaw", identity(Kind::Directory, 4));
            Ok(())
        }).unwrap();
        let with_mode = |mode: u32| Identity { mode: libc::S_IFDIR | mode, ..identity(Kind::Directory, 4) };
        // The installer re-applies 0700: exactly that transition is re-recorded durably.
        world.borrow_mut().put("/opt/duduclaw", with_mode(0o700));
        let saves = world.borrow().saved.len();
        recorder.rerecord_mode("/opt/duduclaw", 0o700).unwrap();
        assert_eq!(world.borrow().saved.len(), saves + 1);
        assert_eq!(recorder.ledger().entry("/opt/duduclaw").unwrap().identity, Some(with_mode(0o700)));
        recorder.intend(&[("/opt/duduclaw", Kind::Directory)]).unwrap();
        recorder.confirm(&["/opt/duduclaw"]).unwrap();
        // Idempotent: nothing to re-record once recorded.
        let saves = world.borrow().saved.len();
        recorder.rerecord_mode("/opt/duduclaw", 0o700).unwrap();
        assert_eq!(world.borrow().saved.len(), saves);
        // Another party widens it: no re-record matches, the identity differs, the re-intent refuses.
        world.borrow_mut().put("/opt/duduclaw", with_mode(0o777));
        recorder.rerecord_mode("/opt/duduclaw", 0o755).unwrap();
        assert_eq!(recorder.ledger().entry("/opt/duduclaw").unwrap().identity, Some(with_mode(0o700)));
        assert!(recorder.intend(&[("/opt/duduclaw", Kind::Directory)]).is_err());
        // Another object with the installer's mode is still another object.
        world.borrow_mut().put("/opt/duduclaw", Identity { inode: 40, ..with_mode(0o755) });
        recorder.rerecord_mode("/opt/duduclaw", 0o755).unwrap();
        assert_eq!(recorder.ledger().entry("/opt/duduclaw").unwrap().identity, Some(with_mode(0o700)));
        // Files never take this path.
        assert!(recorder.rerecord_mode(UNIT_PATH, 0o644).is_ok(), "no entry: nothing to do");
        recorder.record(&[(UNIT_PATH, Kind::File)], || Ok(())).unwrap();
        assert!(recorder.rerecord_mode(UNIT_PATH, 0o644).is_err());
    }

    #[test]
    fn ledger_classifies_replaced_file_as_other() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("unit");
        fs::write(&path, b"first").unwrap();
        let dir = fs::File::open(directory.path()).unwrap();
        let boot = boot_id().unwrap();
        let Some(recorded) = identity_at(&dir, c"unit", Kind::File).unwrap() else { panic!("present") };
        let entry = Entry { path: UNIT_PATH.into(), kind: Kind::File, state: State::Created, identity: Some(recorded), keep: Vec::new() };
        assert_eq!(classify_at(&dir, c"unit", &entry, &boot).unwrap(), Now::Recorded);
        // Same bytes, replaced by rename (a package or an administrator): another inode.
        fs::write(directory.path().join("unit.new"), b"first").unwrap();
        fs::rename(directory.path().join("unit.new"), &path).unwrap();
        assert_eq!(classify_at(&dir, c"unit", &entry, &boot).unwrap(), Now::Other, "a new inode is another object");
        fs::remove_file(&path).unwrap();
        assert_eq!(classify_at(&dir, c"unit", &entry, &boot).unwrap(), Now::Absent);
        // Without a kernel mount id there is no identity at all (no st_dev fallback).
        fs::write(&path, b"x").unwrap();
        MOUNT_ID_SEAM.with(|seam| *seam.borrow_mut() = Some(Box::new(|_: &CStr, _: Option<u64>| None)));
        let unavailable = identity_at(&dir, c"unit", Kind::File);
        MOUNT_ID_SEAM.with(|seam| *seam.borrow_mut() = None);
        assert!(unavailable.is_err());
    }
}
