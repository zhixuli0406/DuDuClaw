//! R8r-2: the employee's model connection document, written by root inside
//! the employee's own home: `home/.duduclaw-api/providers.json` of its
//! storage image (`/home/employee/.duduclaw-api/providers.json` for the
//! sandboxed OpenAI-compatible runtime, which is its only reader).
//!
//! Every function starts from the VERIFIED home directory descriptor of the
//! temporarily attached image (`LoopStorage`: uid:uid, 0700, on the image's
//! device) and resolves beneath it only (openat2 RESOLVE_BENEATH |
//! NO_SYMLINKS | NO_MAGICLINKS | NO_XDEV, through
//! `duduclaw_migration::import_blobs`). Root never opens or reads an existing
//! `providers.json`: it creates a new file and renames it over the name.
//! Taking the descriptor and the expected owner as arguments keeps every rule
//! testable unprivileged (a temporary directory and the test user).
use std::{fs::File, io, os::fd::OwnedFd};
use duduclaw_migration::import_blobs::{self as blobs, Owner, ReplaceError};
use crate::storage::{valid_providers_temp, ProvidersFailure};

/// The employee-owned directory (0700, uid:gid of the employee).
pub(crate) const DIRECTORY: &str = ".duduclaw-api";
/// The document the runtime reads.
pub(crate) const FILE: &str = "providers.json";

fn failure(error: ReplaceError) -> ProvidersFailure {
    match error {
        ReplaceError::Unsafe => ProvidersFailure::HomeEntryUnsafe,
        ReplaceError::NoSpace => ProvidersFailure::NoSpace,
        ReplaceError::Io => ProvidersFailure::Unavailable,
        ReplaceError::TempLeftBehind => ProvidersFailure::TempLeftBehind,
    }
}
/// Writes `bytes` (every call: a new file, never "unchanged ⇒ skip").
pub(crate) fn write(home: &OwnedFd, owner: Owner, temp: &str, bytes: &[u8]) -> Result<(), ProvidersFailure> {
    write_with(home, owner, temp, bytes.len() as u64, |file| io::Write::write_all(file, bytes))
}
/// [`write`] with the file contents produced by `fill` (exactly `len` bytes).
/// 1. `.duduclaw-api`: an existing one must be a directory owned by `owner`,
///    mode exactly 0700, on the home's mount (else `HomeEntryUnsafe`, nothing
///    written); an absent one is created 0700 and given to `owner`.
/// 2. `temp` (fixed prefix + 16 hex) is created O_CREAT|O_EXCL|O_NOFOLLOW,
///    checked regular with one link, written, made 0600 and `owner`'s,
///    fsynced; 3. renamed over `providers.json` (a directory there is
///    `HomeEntryUnsafe`), the directory fsynced; 4. metadata read back.
///    5. Any failure before the rename removes `temp`; if that removal
///    fails, `TempLeftBehind` (the caller keeps its maintenance marker).
pub(crate) fn write_with(home: &OwnedFd, owner: Owner, temp: &str, len: u64,
    fill: impl FnOnce(&mut File) -> io::Result<()>) -> Result<(), ProvidersFailure> {
    if !valid_providers_temp(temp) { return Err(ProvidersFailure::Unavailable); }
    let directory = blobs::open_private_directory(home, DIRECTORY, owner, true).map_err(failure)?
        .ok_or(ProvidersFailure::Unavailable)?;
    blobs::replace_with(&directory, temp, FILE, len, owner, fill).map_err(failure)
}
/// `clear`: the same directory rule (absent ⇒ nothing to do); `providers.json`
/// is unlinked without being opened (absent ⇒ success); `.duduclaw-api` is
/// removed only if it is then empty (never recursively).
pub(crate) fn clear(home: &OwnedFd, owner: Owner) -> Result<(), ProvidersFailure> {
    let Some(directory) = blobs::open_private_directory(home, DIRECTORY, owner, false).map_err(failure)? else { return Ok(()) };
    blobs::remove_entry(&directory, FILE).map_err(failure)?;
    drop(directory);
    blobs::remove_empty_directory(home, DIRECTORY).map_err(failure)
}
/// Start-up recovery of an unfinished maintenance: remove the recorded temp
/// file. An absent or unsafe `.duduclaw-api` cannot hold a temp file root
/// created there (root only creates one through a verified directory, and
/// no run attaches while the marker exists): nothing to remove. Otherwise
/// `Ok` only when the recorded name was removed or is confirmed absent
/// (ENOENT); a directory there, EPERM or any I/O failure is an error, so the
/// caller keeps the marker (F4: never cleared without confirmation).
pub(crate) fn remove_temp(home: &OwnedFd, owner: Owner, temp: &str) -> Result<(), ProvidersFailure> {
    if !valid_providers_temp(temp) { return Err(ProvidersFailure::Unavailable); }
    match blobs::open_private_directory(home, DIRECTORY, owner, false) {
        Ok(Some(directory)) => match blobs::remove_entry(&directory, temp) {
            Ok(()) => Ok(()),
            Err(_) => Err(ProvidersFailure::Unavailable),
        },
        Ok(None) | Err(ReplaceError::Unsafe) => Ok(()),
        Err(error) => Err(failure(error)),
    }
}

#[cfg(test)]
#[path = "providers_file_tests.rs"]
mod tests;
