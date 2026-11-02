//! A locked file whose acquiring process explicitly unlocks before closing.
use std::fs::File;

#[derive(Debug)]
pub struct OwnerLock {
    file: File,
    pid: u32,
}

impl OwnerLock {
    /// Wrap an already locked file. Acquisition policy belongs to the caller.
    pub fn new(file: File) -> Self {
        Self { file, pid: std::process::id() }
    }
}

impl Drop for OwnerLock {
    fn drop(&mut self) {
        // CLOEXEC only closes inherited descriptors at exec. Explicit unlock
        // releases the open-file-description lock even while a child retains
        // its copy. A child must never unlock its parent's active guard.
        if self.pid == std::process::id() {
            let _ = fs2::FileExt::unlock(&self.file);
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use fs2::FileExt;

    #[test]
    fn owner_drop_unlocks_even_while_another_descriptor_is_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("owner.lock");
        let file = File::create(&path).unwrap();
        file.try_lock_exclusive().unwrap();
        // dup shares the open file description, just like fork inheritance.
        // Avoid forking a multithreaded test process to test this lifetime.
        let inherited = file.try_clone().unwrap();
        let guard = OwnerLock::new(file);
        let contender = File::options().read(true).write(true).open(&path).unwrap();
        assert!(contender.try_lock_exclusive().is_err());
        drop(guard);
        contender.try_lock_exclusive().expect("parent drop must unlock before inherited fd closes");
        drop(inherited);
    }
}
