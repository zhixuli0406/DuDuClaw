//! On-disk access for `<home>/session_pending/` — the only code that touches
//! ledger files, lock sidecars or temp files.
//!
//! Every access goes through a directory handle ([`crate::fs_safe::SafeDir`])
//! and is relative to it, so there is no check-then-open on a path:
//!
//! * the directory is opened without following a symlink, must be owned by
//!   the gateway user and is tightened to 0700 on every open (read or write);
//! * a ledger or lock is opened with `O_NOFOLLOW | O_NONBLOCK` (a planted
//!   symlink is refused, a FIFO never blocks the open), then the **open
//!   handle** is checked: regular file, single hard link, owned by the gateway
//!   user, and tightened to 0600 ([`open_private_file`]);
//! * a ledger is replaced through a fresh `O_CREAT|O_EXCL|O_NOFOLLOW` 0600
//!   temp file in the same directory, fsync, then an fd-relative rename;
//! * the lock sidecar is opened by the same rule and locked by handle
//!   ([`duduclaw_core::fs_lock::lock_handle_within`]); after the wait the
//!   locked handle must still be the inode linked at the name, and the sidecar
//!   is unlinked while still held, so lock files never accumulate;
//! * a lock acquisition has **one** deadline fixed on entry ([`Deadline`]);
//!   each retry after an inode change waits only for the time left, and a
//!   retry after expiry is `TimedOut` at once. A turn shares one deadline
//!   between its ledger lock and the admission lock.
//!
//! Ownership: every ledger belongs to one principal — the verifiable sender
//! identity the caller derives (`super::owner`: the authenticated WebChat
//! account, otherwise channel + sender id; never a session or `conv` value a
//! caller can choose). The file name is `<owner>-<key>.json`: a fixed 32-hex
//! owner tag ([`owner_tag`]) then the 64-hex ledger key (of `(session,
//! sender)`, or for a WebChat account of `(account, employee, session)` —
//! [`account_ledger_file`]), so ownership is read
//! from the directory listing without opening anything. The lock sidecar
//! (`<ledger>.lock`) and temp files (`.<ledger>.tmp-<32 hex>`) carry the same
//! prefix. Names of the earlier unreleased format (`<64 hex>.json`) belong to
//! nobody and are never read: GC removes them, and an at-cap sweep may
//! reclaim them like expired ledgers.
//!
//! Admission: creating a NEW ledger happens only under the fixed
//! [`ADMISSION_LOCK`] sidecar (same safe open, lock by handle), held just
//! while counting and creating, so concurrent creations are serialised and
//! never exceed either limit:
//!
//! * per principal, at most [`MAX_LEDGERS_PER_OWNER`] (16) ledgers. At that
//!   quota the principal's own expired ledgers are removed first, then its
//!   own least recently written ledger is evicted, then the new one admitted;
//! * globally, at most [`MAX_LEDGERS`] (4096). At the cap, expired ledgers of
//!   ANY principal are reclaimed first (the whole bounded listing is checked,
//!   least recently written first); if the directory is still full, the
//!   newcomer may evict only its own ledgers, and one with none is refused —
//!   no ledger for that turn, logged once per streak. Another principal's
//!   unexpired ledger is never deleted, so with 4096 / 16 at least 256
//!   distinct principals are needed to fill the directory.
//!
//! An eviction decides on a snapshot (mtime and size) taken before the
//! victim's lock; after the zero-wait lock it re-reads both from the
//! validated, locked handle and skips a ledger that changed meanwhile,
//! re-queuing it by its fresh snapshot (at most [`MAX_REPICKS`] times per
//! sweep). A ledger somebody holds is skipped, never waited for. Every
//! directory scan reads at most [`MAX_SCAN_ENTRIES`] entries
//! ([`SafeDir::list_bounded`]); an over-full listing refuses admission until
//! GC (which resumes where its last pass stopped) has caught up.
//!
//! Windows: the same handle-relative opens (NtCreateFile with
//! `FILE_OPEN_REPARSE_POINT`; reparse points, non-files and hard links are
//! refused), but there is no owner-uid check and no 0700/0600 tightening —
//! access relies on the ACL inherited from the gateway home. The lock sidecar
//! cannot be unlinked while a handle is open, so it is removed after release
//! (a still-open handle elsewhere makes that removal fail, which is safe) and
//! the inode re-check after the wait is skipped. `SafeDir` data handles (the
//! ledger and its lock sidecar) never share DELETE, so a lock-free reader
//! holding a ledger open would make the writer's replace or delete fail with
//! a sharing violation; Windows readers therefore read under the ledger lock
//! ([`read_for_reminder`]) with the turn bound, and on timeout the reminder is
//! skipped for that turn. The attribute-only opens behind `entry`, `modified`
//! and listing request no data access and also share DELETE, so a name
//! inspected before a lock (or by an admission scan) never blocks a replace or
//! delete. Remaining platform difference: Unix readers are lock-free and never
//! wait or create a sidecar; Windows readers may wait up to
//! [`TURN_LOCK_WAIT`] and create/remove a sidecar per read. Other non-Unix
//! targets refuse every ledger operation (`SafeDir` has no no-follow reads
//! there).

use std::io::{self, Read};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime};

use duduclaw_core::fs_lock::{lock_handle_within, FileLockGuard};
use sha2::{Digest, Sha256};

use super::{Item, MAX_FILE_BYTES};
use crate::fs_safe::{refused, EntryKind, SafeDir};

pub(super) const DIR_NAME: &str = "session_pending";
/// Global cap on ledger files. Each is at most `MAX_FILE_BYTES` (64 KiB), so
/// the directory is bounded at ~256 MiB and 4096 data inodes (+ transient
/// locks). At the cap expired ledgers are reclaimed; a newcomer may then
/// evict only its own principal's ledgers (see module docs).
pub(super) const MAX_LEDGERS: usize = 4096;
/// Hard per-principal quota: one sender with many sessions (e.g. WebChat
/// `conv` nonces) replaces only its own ledgers. `MAX_LEDGERS /
/// MAX_LEDGERS_PER_OWNER` = 256 principals are needed to fill the directory.
pub(super) const MAX_LEDGERS_PER_OWNER: usize = 16;
/// Hex digits of the owner tag that prefixes every ledger name (128 bits).
const OWNER_HEX: usize = 32;
/// Hex digits of the ledger key (both key forms).
const KEY_HEX: usize = 64;

/// The two admission limits; production uses [`LIMITS`], tests smaller ones.
#[derive(Clone, Copy, Debug)]
pub(super) struct Limits {
    pub(super) global: usize,
    pub(super) per_owner: usize,
}

pub(super) const LIMITS: Limits = Limits { global: MAX_LEDGERS, per_owner: MAX_LEDGERS_PER_OWNER };
/// Bound on a turn's wait for the ledger lock; on timeout the ledger is
/// skipped for that turn and the conversation carries on.
pub(super) const TURN_LOCK_WAIT: Duration = Duration::from_secs(2);
/// Fixed sidecar serialising the creation of new ledgers.
pub(super) const ADMISSION_LOCK: &str = "admission.lock";
/// Most directory entries one scan (admission count or GC pass) reads. A
/// legitimate directory holds at most `MAX_LEDGERS` ledgers plus transient
/// locks and temps, well below this.
pub(super) const MAX_SCAN_ENTRIES: usize = 16_384;
/// How many of its own live ledgers a newcomer tries to evict (each a
/// non-blocking lock try) before it is skipped for this turn.
const EVICTION_ATTEMPTS: usize = 64;
/// Per sweep: how often a candidate whose locked re-check shows it changed
/// since the snapshot is re-queued by its fresh snapshot (then it is skipped).
pub(super) const MAX_REPICKS: usize = 64;
/// Safety bound on the rounds of one lock acquisition; the deadline is the
/// real bound. Every release unlinks the sidecar, so each waiter on a busy
/// lock (the admission lock above all) uses one round per holder it waits
/// out — a small fixed count would fail waiters long before the deadline.
const MAX_LOCK_ROUNDS: usize = 64;
/// Whether reminder reads take the ledger lock (see module docs).
const READERS_LOCK: bool = cfg!(windows);

static CAP_LOGGED: AtomicBool = AtomicBool::new(false);
static REFUSAL_LOGGED: AtomicBool = AtomicBool::new(false);

/// The fixed-length owner tag of principal `(kind, id)`: the first 128 bits
/// of `sha256(json(domain, kind, id))` as lowercase hex. `kind` is the
/// channel (or `webchat-account`); length-delimited JSON avoids ambiguous
/// concatenations.
pub(super) fn owner_tag(kind: &str, id: &str) -> String {
    let key = serde_json::to_vec(&("duduclaw.pending_owner.v1", kind, id)).expect("string triple serialization");
    let digest = Sha256::digest(key);
    hex::encode(&digest[..OWNER_HEX / 2])
}

/// `<owner>-<sha256(json(session, sender))>.json`. The owner prefix makes the
/// principal checkable from a listing; the hex name cannot traverse.
pub(super) fn ledger_file(owner: &str, session: &str, sender: &str) -> String {
    let key = serde_json::to_vec(&(session, sender)).expect("string pair serialization");
    format!("{owner}-{:x}.json", Sha256::digest(key))
}

/// The WebChat-account form of [`ledger_file`]:
/// `<owner>-<sha256(json(domain, account, agent, session))>.json`. Keyed on
/// the authenticated account, the employee and the stored conversation's
/// session id — values a reconnect keeps — and never on the per-connection
/// sender id. The same fixed-length hex name; the domain string keeps it
/// apart from every `(session, sender)` key.
pub(super) fn account_ledger_file(owner: &str, account: &str, agent: &str, session: &str) -> String {
    let key = serde_json::to_vec(&("duduclaw.pending_key.webchat_account.v1", account, agent, session))
        .expect("string tuple serialization");
    format!("{owner}-{:x}.json", Sha256::digest(key))
}

pub(super) fn lock_name(ledger: &str) -> String {
    format!("{ledger}.lock")
}

fn is_lower_hex(text: &str, len: usize) -> bool {
    text.len() == len && text.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The owner tag of ledger file `name`, `None` for anything else.
pub(super) fn ledger_owner(name: &str) -> Option<&str> {
    let (owner, key) = name.strip_suffix(".json")?.split_once('-')?;
    (is_lower_hex(owner, OWNER_HEX) && is_lower_hex(key, KEY_HEX)).then_some(owner)
}

pub(super) fn is_ledger_name(name: &str) -> bool {
    ledger_owner(name).is_some()
}

/// A ledger name of the earlier unreleased format, `<64 hex>.json`: owned by
/// nobody and read by no code path.
pub(super) fn is_legacy_ledger_name(name: &str) -> bool {
    name.strip_suffix(".json").is_some_and(|key| is_lower_hex(key, KEY_HEX))
}

fn is_any_ledger_name(name: &str) -> bool {
    is_ledger_name(name) || is_legacy_ledger_name(name)
}

/// A ledger's lock sidecar (current or legacy ledger name).
pub(super) fn is_lock_name(name: &str) -> bool {
    name.strip_suffix(".lock").is_some_and(is_any_ledger_name)
}

/// The temp name `fs_safe::tmp_name` gives a ledger: `.<ledger>.tmp-<32 hex>`.
pub(super) fn is_temp_name(name: &str) -> bool {
    let Some(rest) = name.strip_prefix('.') else { return false };
    let Some((ledger, suffix)) = rest.split_once(".tmp-") else { return false };
    is_any_ledger_name(ledger) && suffix.len() == 32 && suffix.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Open `<home>/session_pending` without following a symlink. `create` makes
/// it (0700) when absent; otherwise an absent directory is `Ok(None)`.
/// Unix: the directory must be owned by the effective uid and is tightened to
/// 0700 through its descriptor on every open, read or write.
pub(super) fn open_dir(home: &std::path::Path, create: bool) -> io::Result<Option<SafeDir>> {
    let root = match SafeDir::open_root(home) {
        Ok(root) => root,
        Err(e) if !create && e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let Some(dir) = root.child_dir(DIR_NAME, create)? else { return Ok(None) };
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let meta = dir.metadata()?;
        if !meta.is_dir() || meta.uid() != nix::unistd::geteuid().as_raw() {
            return Err(refused("pending ledger directory is not owned by the gateway user"));
        }
        if meta.mode() & 0o7777 != 0o700 {
            dir.set_mode(0o700)?;
        }
    }
    Ok(Some(dir))
}

/// Verify an already-open handle: regular file, one hard link, owned by the
/// gateway user (Unix), and tighten it to 0600 through the handle.
fn verify_private(file: &std::fs::File) -> io::Result<()> {
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(refused("pending ledger entry is not a regular file"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if meta.nlink() != 1 {
            return Err(refused("pending ledger entry has several hard links"));
        }
        if meta.uid() != nix::unistd::geteuid().as_raw() {
            return Err(refused("pending ledger entry is not owned by the gateway user"));
        }
        if meta.mode() & 0o7777 != 0o600 {
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
    }
    Ok(())
}

/// The one safe open used for every ledger and lock read (and before every
/// write): `O_NOFOLLOW | O_NONBLOCK` relative to the pinned directory, then
/// [`verify_private`] on the open handle. `Ok(None)` when absent; a symlink,
/// FIFO, socket, device, directory, hard-linked or foreign-owned entry is
/// refused without being followed, read or modified.
pub(super) fn open_private_file(dir: &SafeDir, name: &str) -> io::Result<Option<std::fs::File>> {
    let Some(file) = dir.open_read_checked(name, true)? else { return Ok(None) };
    verify_private(&file)?;
    Ok(Some(file))
}

/// Read and parse a ledger. `Ok(None)` when absent; a parse failure is
/// `InvalidData`; anything refused by [`open_private_file`] is an error.
pub(super) fn read_items(dir: &SafeDir, name: &str) -> io::Result<Option<Vec<Item>>> {
    let Some(file) = open_private_file(dir, name)? else { return Ok(None) };
    parse_ledger(file).map(Some)
}

/// Parse a ledger from an already validated handle, which is consumed (and
/// closed) so a following delete on Windows is not blocked by it.
fn parse_ledger(file: std::fs::File) -> io::Result<Vec<Item>> {
    if file.metadata()?.len() > MAX_FILE_BYTES {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "oversized pending ledger"));
    }
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "oversized pending ledger"));
    }
    serde_json::from_slice(&bytes).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// Replace a ledger atomically. The caller holds the ledger lock and has just
/// read the current entry through [`open_private_file`] (which refused a
/// symlink / non-regular / hard-linked entry and tightened it to 0600), so the
/// mode `write_file_atomic` carries over is 0600; the result is re-verified.
pub(super) fn write_items(dir: &SafeDir, name: &str, items: &[Item]) -> io::Result<()> {
    let json = serde_json::to_string(items).map_err(io::Error::other)?;
    if json.len() as u64 > MAX_FILE_BYTES {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "oversized pending ledger"));
    }
    dir.write_file_atomic(name, &json)?;
    open_private_file(dir, name)?.ok_or_else(|| io::Error::other("pending ledger vanished after write"))?;
    Ok(())
}

/// A reminder's read of a ledger. Unix: lock-free (writers replace by atomic
/// rename, so one whole version is seen). Windows: under the ledger lock with
/// the turn bound (see module docs); a timeout is an error, which the caller
/// turns into "no reminder this turn".
pub(super) fn read_for_reminder(dir: &SafeDir, name: &str) -> io::Result<Option<Vec<Item>>> {
    read_shared(dir, name, READERS_LOCK.then_some(TURN_LOCK_WAIT))
}

/// [`read_items`], under the ledger lock when `lock_wait` is set. An absent
/// ledger is never locked, so a read creates no sidecar for nothing.
pub(super) fn read_shared(dir: &SafeDir, name: &str, lock_wait: Option<Duration>) -> io::Result<Option<Vec<Item>>> {
    let Some(wait) = lock_wait else { return read_items(dir, name) };
    if dir.entry(name)?.is_none() {
        return Ok(None);
    }
    let Some(held) = lock(dir, &lock_name(name), true, wait)? else { return Ok(None) };
    let result = read_items(dir, name);
    held.release(dir);
    result
}

/// Remove a ledger (the directory entry only). Windows refuses to delete a
/// file another handle has open; then an empty ledger is written instead so
/// a completed receipt still takes effect, and GC removes it later.
pub(super) fn remove_ledger(dir: &SafeDir, name: &str) -> io::Result<()> {
    match dir.remove_file(name) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(_) => dir.write_file_atomic(name, "[]"),
    }
}

/// Create the NEW ledger `name` (the caller holds its ledger lock and saw no
/// file) under the global admission lock, making room within `limits` (see
/// module docs). The admission wait uses what is left of the turn's
/// `deadline`. `Ok(())` without a file when admission is impossible this turn
/// (the conversation carries on). A name without an owner prefix is refused.
pub(super) fn create_ledger(dir: &SafeDir, name: &str, items: &[Item], limits: Limits, now: i64, deadline: Deadline) -> io::Result<()> {
    let Some(owner) = ledger_owner(name) else {
        return Err(refused("pending ledger name carries no owner"));
    };
    let Some(admission) = lock_until(dir, ADMISSION_LOCK, true, deadline)? else {
        return Err(io::Error::other("pending ledger admission lock unavailable"));
    };
    let result = (|| -> io::Result<()> {
        if !admit(dir, name, owner, limits, now)? {
            return Ok(());
        }
        write_items(dir, name, items)
    })();
    admission.release(dir);
    result
}

/// What an eviction decides on: a ledger's mtime and size, read without
/// opening its data. Orders least recently written first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Snapshot {
    pub(super) modified: SystemTime,
    pub(super) len: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Candidate<'a> {
    pub(super) snapshot: Snapshot,
    pub(super) name: &'a str,
}

/// What one eviction try did.
#[derive(Debug)]
pub(super) enum Evicted {
    /// The slot is free (removed, or already gone).
    Freed,
    /// Busy, still live (expiry sweep), refused by the safe open, or not removable.
    Kept,
    /// Rewritten or replaced since the snapshot; carries the fresh one.
    Changed(Snapshot),
}

/// Whether one more ledger fits for `owner`, making room within `limits`
/// (see module docs). Runs under the admission lock, so no other ledger can
/// be created meanwhile and both counts can only fall. Another principal's
/// ledger is removed only when expired (or ownerless legacy). Logs once per
/// cap breach and once per refusal streak.
fn admit(dir: &SafeDir, name: &str, owner: &str, limits: Limits, now: i64) -> io::Result<bool> {
    let (entries, truncated) = dir.list_bounded(MAX_SCAN_ENTRIES)?;
    if truncated || limits.global == 0 || limits.per_owner == 0 {
        // Far beyond any legitimate state: the counts cannot be proven below
        // the limits, so fail closed for this turn and let GC catch up.
        log_refusal(limits, "pending ledger directory is over-full or a limit is zero; the new ledger is skipped this turn");
        return Ok(false);
    }
    let ledgers: Vec<(&str, u64)> = entries.iter()
        .filter(|e| e.kind == EntryKind::File && e.name != name && is_any_ledger_name(&e.name))
        .map(|e| (e.name.as_str(), e.size))
        .collect();
    let own_count = ledgers.iter().filter(|(victim, _)| ledger_owner(victim) == Some(owner)).count();
    let mut need_own = (own_count + 1).saturating_sub(limits.per_owner);
    let mut need_all = (ledgers.len() + 1).saturating_sub(limits.global);
    if need_own == 0 && need_all == 0 {
        CAP_LOGGED.store(false, Ordering::Relaxed);
        REFUSAL_LOGGED.store(false, Ordering::Relaxed);
        return Ok(true);
    }
    if need_all > 0 && !CAP_LOGGED.swap(true, Ordering::Relaxed) {
        tracing::warn!(count = ledgers.len(), cap = limits.global,
            "pending ledger cap reached; reclaiming expired ledgers, then only the newcomer's own");
    }
    let mut own = Vec::new();
    let mut others = Vec::new();
    for (victim, len) in ledgers {
        let mine = ledger_owner(victim) == Some(owner);
        match dir.modified(victim) {
            Ok(Some(modified)) => {
                let candidate = Candidate { snapshot: Snapshot { modified, len }, name: victim };
                if mine { own.push(candidate) } else { others.push(candidate) }
            }
            // Vanished since the listing: its slot is already free.
            Ok(None) => {
                need_all = need_all.saturating_sub(1);
                if mine {
                    need_own = need_own.saturating_sub(1);
                }
            }
            // Cannot even be inspected without following: not evictable.
            Err(_) => {}
        }
    }
    own.sort();
    others.sort();
    // 1. The newcomer's own expired ledgers (they count against both limits).
    let own_budget = own.len() + MAX_REPICKS;
    let freed = sweep(dir, &mut own, Some(now), need_own.max(need_all), own_budget);
    need_own = need_own.saturating_sub(freed);
    need_all = need_all.saturating_sub(freed);
    // 2. Still at the global cap: expired (or ownerless legacy) ledgers of
    //    every other principal, across the whole bounded listing.
    if need_all > 0 {
        let others_budget = others.len() + MAX_REPICKS;
        let freed = sweep(dir, &mut others, Some(now), need_all, others_budget);
        need_all = need_all.saturating_sub(freed);
    }
    // 3. The newcomer's own least recently written live ledgers — never
    //    another principal's.
    if need_own > 0 || need_all > 0 {
        let freed = sweep(dir, &mut own, None, need_own.max(need_all), EVICTION_ATTEMPTS);
        if freed > 0 {
            tracing::debug!(freed, "pending ledger: the sender's own oldest ledgers evicted to admit a new one");
        }
        need_own = need_own.saturating_sub(freed);
        need_all = need_all.saturating_sub(freed);
    }
    if need_own > 0 || need_all > 0 {
        log_refusal(limits, "pending ledger limit reached and no expired ledger or ledger of this sender could be removed; the new ledger is skipped this turn");
        return Ok(false);
    }
    REFUSAL_LOGGED.store(false, Ordering::Relaxed);
    Ok(true)
}

fn log_refusal(limits: Limits, message: &'static str) {
    if !REFUSAL_LOGGED.swap(true, Ordering::Relaxed) {
        tracing::warn!(cap = limits.global, per_owner = limits.per_owner, "{message}");
    }
}

/// Evict up to `want` ledgers from `pool` (sorted least recently written
/// first), trying at most `max_tries` times. `expired_at`: only ledgers whose
/// items have all expired then (or that no longer parse, or ownerless legacy
/// ones); `None`: whatever they hold. A freed candidate leaves the pool; one
/// that changed since its snapshot is re-queued by its fresh snapshot, at
/// most [`MAX_REPICKS`] times, then skipped. Returns the slots freed.
fn sweep(dir: &SafeDir, pool: &mut Vec<Candidate<'_>>, expired_at: Option<i64>, want: usize, max_tries: usize) -> usize {
    let (mut freed, mut tries, mut repicks, mut at) = (0, 0, 0, 0);
    while freed < want && at < pool.len() && tries < max_tries {
        tries += 1;
        let candidate = pool[at];
        match try_evict(dir, &candidate, expired_at) {
            Evicted::Freed => {
                pool.remove(at);
                freed += 1;
            }
            Evicted::Changed(fresh) if repicks < MAX_REPICKS => {
                repicks += 1;
                pool.remove(at);
                let requeued = Candidate { snapshot: fresh, ..candidate };
                let slot = pool.partition_point(|c| *c < requeued);
                pool.insert(slot, requeued);
                // Re-queued before the cursor: the next untried one moved up.
                if slot < at {
                    at += 1;
                }
            }
            Evicted::Changed(_) | Evicted::Kept => at += 1,
        }
    }
    freed
}

/// One zero-wait eviction try of `candidate` (see [`sweep`]). Once the
/// victim's lock is held, its mtime and size are re-read from the validated,
/// locked handle; any difference from the snapshot means it was rewritten or
/// replaced after the decision, and nothing is removed. A ledger the safe
/// open refuses is left alone.
pub(super) fn try_evict(dir: &SafeDir, candidate: &Candidate<'_>, expired_at: Option<i64>) -> Evicted {
    let victim = candidate.name;
    let Ok(Some(held)) = lock(dir, &lock_name(victim), true, Duration::ZERO) else { return Evicted::Kept };
    let outcome = (|| -> io::Result<Evicted> {
        let Some(file) = open_private_file(dir, victim)? else { return Ok(Evicted::Freed) };
        let meta = file.metadata()?;
        let fresh = Snapshot { modified: meta.modified()?, len: meta.len() };
        if fresh != candidate.snapshot {
            return Ok(Evicted::Changed(fresh));
        }
        // The handle is consumed (closed) on every path before the delete,
        // which Windows would otherwise refuse while it is open.
        let live = match expired_at {
            Some(now) if !is_legacy_ledger_name(victim) => match parse_ledger(file) {
                Ok(mut items) => {
                    super::prune(&mut items, now);
                    !items.is_empty()
                }
                // Our own verified file that no longer parses holds nothing usable.
                Err(e) if e.kind() == io::ErrorKind::InvalidData => false,
                Err(e) => return Err(e),
            },
            _ => {
                drop(file);
                false
            }
        };
        if live {
            return Ok(Evicted::Kept);
        }
        // A plain removal: the `[]` fallback of `remove_ledger` would not
        // free a slot, so a failed delete just means "not evicted".
        match dir.remove_file(victim) {
            Ok(()) => Ok(Evicted::Freed),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Evicted::Freed),
            Err(e) => Err(e),
        }
    })();
    held.release(dir);
    outcome.unwrap_or(Evicted::Kept)
}

/// A held ledger lock. Dropping it without [`LedgerLock::release`] still
/// unlocks (the guard), but leaves the sidecar for GC.
pub(super) struct LedgerLock {
    guard: FileLockGuard,
    name: String,
}

impl LedgerLock {
    /// Unlock and remove the sidecar. Unix: unlinked while still held, so a
    /// waiter that then acquires the old inode sees it is no longer linked
    /// and retries on a fresh one. Windows: removed after release; deletion
    /// fails while any other handle is open, which keeps a live lock.
    pub(super) fn release(self, dir: &SafeDir) {
        #[cfg(unix)]
        {
            let _ = dir.remove_file(&self.name);
            drop(self.guard);
        }
        #[cfg(not(unix))]
        {
            drop(self.guard);
            let _ = dir.remove_file(&self.name);
        }
    }
}

/// Is the locked handle still the inode linked at `name`?
#[cfg(unix)]
fn still_linked(dir: &SafeDir, name: &str, held: &std::fs::File) -> io::Result<bool> {
    use std::os::unix::fs::MetadataExt;
    let Some(current) = dir.open_read_checked(name, true)? else { return Ok(false) };
    let (a, b) = (held.metadata()?, current.metadata()?);
    Ok(a.dev() == b.dev() && a.ino() == b.ino())
}

#[cfg(not(unix))]
fn still_linked(_dir: &SafeDir, _name: &str, _held: &std::fs::File) -> io::Result<bool> {
    // No stable file-id API without extra features; Windows deletion
    // semantics (see module docs) keep a locked sidecar linked.
    Ok(true)
}

/// One hard deadline for a lock acquisition, fixed before the first try.
#[derive(Clone, Copy, Debug)]
pub(super) struct Deadline(Instant);

impl Deadline {
    /// `wait` from now.
    pub(super) fn after(wait: Duration) -> Self {
        Self::starting_at(Instant::now(), wait)
    }

    /// `wait` from `start`. An unrepresentable deadline falls back to
    /// `start` (a single try), never to an unbounded wait.
    pub(super) fn starting_at(start: Instant, wait: Duration) -> Self {
        Self(start.checked_add(wait).unwrap_or(start))
    }

    /// The wait one lock round may still use at `now`. The first round always
    /// gets its try (a zero budget is one non-blocking attempt); any later
    /// round at or after the deadline is `TimedOut` immediately.
    pub(super) fn budget(self, now: Instant, first_round: bool) -> io::Result<Duration> {
        let left = self.0.saturating_duration_since(now);
        if left.is_zero() && !first_round {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "lock_wait_timeout: pending ledger lock deadline passed"));
        }
        Ok(left)
    }
}

/// Lock the sidecar `lock` for a ledger. `create` makes a missing sidecar
/// (`O_CREAT|O_EXCL|O_NOFOLLOW`, 0600); otherwise a missing one is
/// `Ok(None)`. A symlink, FIFO or other non-regular entry at the lock name is
/// refused. `wait` bounds the whole acquisition (zero: one try); timeout is
/// `TimedOut`.
pub(super) fn lock(dir: &SafeDir, lock: &str, create: bool, wait: Duration) -> io::Result<Option<LedgerLock>> {
    lock_until(dir, lock, create, Deadline::after(wait))
}

/// [`lock`] bounded by an already running `deadline`.
pub(super) fn lock_until(dir: &SafeDir, lock: &str, create: bool, deadline: Deadline) -> io::Result<Option<LedgerLock>> {
    lock_rounds(dir, lock, create, deadline, &Instant::now, &|_: Duration| {})
}

/// The acquisition loop. `now` is the clock; `after_lock` runs with each
/// round's budget once the handle is locked, before the inode re-check. Both
/// are seams for the deterministic deadline test; production passes
/// `Instant::now` and a no-op.
pub(super) fn lock_rounds(
    dir: &SafeDir,
    lock: &str,
    create: bool,
    deadline: Deadline,
    now: &dyn Fn() -> Instant,
    after_lock: &dyn Fn(Duration),
) -> io::Result<Option<LedgerLock>> {
    for round in 0..MAX_LOCK_ROUNDS {
        let budget = deadline.budget(now(), round == 0)?;
        let file = if create {
            match dir.open_create_new(lock) {
                Ok(file) => file,
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => match dir.open_read_checked(lock, true)? {
                    Some(file) => file,
                    None => continue,
                },
                Err(e) => return Err(e),
            }
        } else {
            match dir.open_read_checked(lock, true)? {
                Some(file) => file,
                None => return Ok(None),
            }
        };
        verify_private(&file)?;
        let guard = lock_handle_within(file, budget)?;
        after_lock(budget);
        if still_linked(dir, lock, guard.file())? {
            return Ok(Some(LedgerLock { guard, name: lock.to_string() }));
        }
        if !create {
            return Ok(None);
        }
    }
    // Busy, like a timeout: callers skip the ledger (GC counts it as busy).
    Err(io::Error::new(io::ErrorKind::TimedOut, "lock_wait_timeout: pending ledger lock kept being replaced"))
}
