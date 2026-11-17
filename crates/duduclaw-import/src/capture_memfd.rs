//! Sealed, bounded anonymous copies opened through SQLite's default Unix VFS.
use std::{fs::File, io::Read};
use rustix::fs::{MemfdFlags, SealFlags, fcntl_add_seals, fcntl_get_seals, memfd_create};
use crate::{Error, Result};

fn required_seals() -> SealFlags {
    SealFlags::SHRINK | SealFlags::GROW | SealFlags::WRITE | SealFlags::SEAL
}

pub(super) fn copy(input: impl Read, expected_bytes: u64) -> Result<(File, u64)> {
    let fd = memfd_create("duduclaw-capture", MemfdFlags::CLOEXEC | MemfdFlags::ALLOW_SEALING)
        .map_err(|_| Error::Io)?;
    // File owns the descriptor and closes it on every success/error path.
    let mut file = File::from(fd);
    let count = std::io::copy(&mut input.take(crate::MAX_SOURCE_BYTES + 1), &mut file)?;
    if count > crate::MAX_SOURCE_BYTES { return Err(Error::Limit); }
    if count != expected_bytes { return Err(Error::Io); }
    fcntl_add_seals(&file, required_seals()).map_err(|_| Error::Io)?;
    verify(&file, count)?;
    Ok((file, count))
}

#[cfg(test)]
#[test]
fn refuses_reader_that_ends_before_expected_length() {
    let result = copy(&b"short"[..], 6);
    assert!(matches!(result, Err(Error::Io)));
}

pub(super) fn verify(file: &File, copied_bytes: u64) -> Result<()> {
    let seals = fcntl_get_seals(file).map_err(|_| Error::Isolation)?;
    // File::metadata uses fstat on this owned descriptor, never a pathname.
    if !seals.contains(required_seals()) || file.metadata()?.len() != copied_bytes {
        return Err(Error::Isolation);
    }
    Ok(())
}
