//! The shared base is populated only by trusted, input-free host invocations.
//! Parser processes receive an independent copy, never shared writable inodes.
use super::*;

#[cfg(unix)]
impl PreparedOffice {
    pub(super) async fn seed_cache(&self, uv: &Path) -> io::Result<()> {
        // Serialize host warm/copy operations across processes. This lock is not
        // exposed to the sandbox. Drop releases it, including on cancellation.
        let root = SafeDir::open(self.base_cache().parent().unwrap())?;
        private(&root)?;
        let lock = match root.create_new("uv-base-v1.lock", 0o600) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => root.read_file("uv-base-v1.lock")?,
            Err(error) => return Err(error),
        };
        tokio::time::timeout(std::time::Duration::from_secs(180), async {
            loop {
                match lock.try_lock() {
                    Ok(()) => return Ok(()),
                    Err(std::fs::TryLockError::WouldBlock) =>
                        tokio::time::sleep(std::time::Duration::from_millis(50)).await,
                    Err(std::fs::TryLockError::Error(error)) => return Err(error),
                }
            }
        }).await.map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "Office base cache is busy"))??;
        if !self.cache_is_warm_at(self.base_cache())? {
            process::warm_base(self, uv).await?;
            self.mark_cache_warm_at(self.base_cache())?;
        }
        self.copy_base_cache()
    }

    fn copy_base_cache(&self) -> io::Result<()> {
        self.verify_before_spawn()?;
        let base = self.base_cache().canonicalize()?;
        let destination = SafeDir::open(self.run_cache())?;
        if !destination.list()?.is_empty() {
            return Err(refused("Office per-run cache must be empty before seeding"));
        }
        copy_tree(&base, self.run_cache(), &base, self.run_cache(), 0)
    }
}

#[cfg(not(unix))]
impl PreparedOffice {
    pub(super) async fn seed_cache(&self, _: &Path) -> io::Result<()> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "Office cache isolation requires Unix"))
    }
}

#[cfg(unix)]
fn copy_tree(source: &Path, destination: &Path, base: &Path, run: &Path, depth: usize) -> io::Result<()> {
    use std::os::unix::fs::{symlink, PermissionsExt};
    if depth > 64 { return Err(refused("Office cache nesting exceeds limit")); }
    let src = SafeDir::open(source)?;
    let dst = SafeDir::open(destination)?;
    for name in src.list()? {
        // Script environments contain absolute Python links and path-specific
        // launchers. Recreate them offline from the copied wheel/index buckets.
        if depth == 0 && (name.starts_with("environments-v") || name.starts_with("office-warm-")) {
            continue;
        }
        let from = source.join(&name);
        let to = destination.join(&name);
        let metadata = std::fs::symlink_metadata(&from)?;
        if metadata.is_dir() {
            dst.mkdir_child(&name, 0o700)?;
            copy_tree(&from, &to, base, run, depth + 1)?;
        } else if metadata.file_type().is_symlink() {
            // uv wheel buckets link to archive entries. Rebase those links;
            // never follow a link outside the trusted cache or into a skipped env.
            let target = from.canonicalize()?;
            let relative = under(&target, base)?;
            if relative.components().next().unwrap().as_os_str().to_string_lossy().starts_with("environments-v") {
                return Err(refused("Office cache link targets an environment"));
            }
            symlink(run.join(relative), &to)?;
        } else if metadata.is_file() {
            let mut input = src.read_file(&name)?;
            let mut output = dst.create_new(&name, 0o600)?;
            io::copy(&mut input, &mut output)?;
            output.set_permissions(std::fs::Permissions::from_mode(metadata.permissions().mode() & 0o777))?;
        } else {
            return Err(refused("Office cache contains a special file"));
        }
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests;
