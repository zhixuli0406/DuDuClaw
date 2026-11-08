//! Windows private state uses pinned directories and owner-restricted DACLs.
use std::{io, path::Path};
use duduclaw_core::windows_private_state::SecureDirectory;
use zeroize::Zeroizing;

pub struct StateDir {
    directory: SecureDirectory,
}

impl StateDir {
    pub fn open(home: &Path) -> io::Result<Self> {
        Ok(Self { directory: SecureDirectory::open(home, super::STATE_DIR)? })
    }
    pub fn lock(&self, name: &str) -> io::Result<std::fs::File> { self.directory.open_lock(name) }
    pub fn read(&self, name: &str) -> io::Result<Option<Zeroizing<Vec<u8>>>> { self.directory.read(name) }
    pub fn write_atomic(&self, name: &str, bytes: &[u8]) -> io::Result<()> { self.directory.write_atomic(name, bytes) }
    pub fn create_new(&self, name: &str, bytes: &[u8]) -> io::Result<()> { self.directory.create_new(name, bytes) }
}
