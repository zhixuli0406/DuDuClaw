use std::collections::HashSet;
use std::path::{Path, PathBuf};

use duduclaw_core::error::{DuDuClawError, Result};
use tracing::warn;

/// Validates container mount paths to prevent exposing sensitive host files.
pub struct MountGuard {
    allowed_paths: HashSet<PathBuf>,
    blocked_patterns: Vec<String>,
}

impl MountGuard {
    /// Create a guard with sensible default blocked patterns.
    pub fn new() -> Self {
        let blocked = vec![
            ".ssh",
            ".gnupg",
            ".env",
            ".aws",
            ".config/gcloud",
            ".docker/config.json",
            "secret.key",
        ];
        Self {
            allowed_paths: HashSet::new(),
            blocked_patterns: blocked.into_iter().map(String::from).collect(),
        }
    }

    /// Explicitly allow a host path to be mounted.
    pub fn allow_path(&mut self, path: PathBuf) {
        self.allowed_paths.insert(path);
    }

    /// Validate that a mount is safe. Returns `Ok(())` on success.
    pub fn validate_mount(&self, host_path: &Path, readonly: bool) -> Result<()> {
        // Resolve symlinks to prevent traversal attacks.
        let resolved = Self::resolve_symlinks(host_path)?;

        if self.is_blocked(&resolved) {
            warn!(
                path = %resolved.display(),
                "blocked mount attempt to sensitive path"
            );
            return Err(DuDuClawError::Security(format!(
                "mount path is blocked: {}",
                resolved.display()
            )));
        }

        if !self.is_path_allowed(&resolved) {
            warn!(
                path = %resolved.display(),
                "mount path not in allow-list"
            );
            return Err(DuDuClawError::Security(format!(
                "mount path not allowed: {}",
                resolved.display()
            )));
        }

        if !readonly {
            warn!(
                path = %resolved.display(),
                "writable mount requested — ensure this is intentional"
            );
        }

        Ok(())
    }

    /// Check whether any component of `path` matches a blocked pattern.
    fn is_blocked(&self, path: &Path) -> bool {
        // Match against individual path components instead of substring
        for component in path.components() {
            let comp_str = component.as_os_str().to_string_lossy();
            for pattern in &self.blocked_patterns {
                if comp_str == pattern.as_str() || comp_str.ends_with(pattern.as_str()) {
                    return true;
                }
            }
        }
        false
    }

    /// Resolve symlinks so that a path like `/tmp/link -> /home/user/.ssh`
    /// is caught by the blocked-pattern check.
    fn resolve_symlinks(path: &Path) -> Result<PathBuf> {
        // Use canonicalize when the path exists; otherwise fall back to the
        // original path (it might be created later by the container runtime).
        match path.canonicalize() {
            Ok(resolved) => Ok(resolved),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(path.to_path_buf()),
            Err(e) => Err(DuDuClawError::Security(format!(
                "failed to resolve path {}: {e}",
                path.display()
            ))),
        }
    }

    /// Check whether `path` (or any ancestor) is in the allow-list.
    fn is_path_allowed(&self, path: &Path) -> bool {
        if self.allowed_paths.contains(path) {
            return true;
        }
        // Also allow sub-paths of an allowed directory.
        for allowed in &self.allowed_paths {
            if path.starts_with(allowed) {
                return true;
            }
        }
        false
    }
}

impl Default for MountGuard {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocked_path_is_rejected() {
        let guard = MountGuard::new();
        let result = guard.validate_mount(Path::new("/home/user/.ssh/id_rsa"), true);
        assert!(result.is_err());
    }

    #[test]
    fn unallowed_path_is_rejected() {
        let guard = MountGuard::new();
        let result = guard.validate_mount(Path::new("/some/random/path"), true);
        assert!(result.is_err());
    }

    #[test]
    fn allowed_path_passes() {
        let mut guard = MountGuard::new();
        guard.allow_path(PathBuf::from("/opt/project"));
        let result = guard.validate_mount(Path::new("/opt/project"), true);
        assert!(result.is_ok());
    }

    #[test]
    fn sub_path_of_allowed_passes() {
        let mut guard = MountGuard::new();
        guard.allow_path(PathBuf::from("/opt/project"));
        let result = guard.validate_mount(Path::new("/opt/project/src/main.rs"), true);
        assert!(result.is_ok());
    }

    #[test]
    fn blocked_overrides_allowed() {
        let mut guard = MountGuard::new();
        guard.allow_path(PathBuf::from("/home/user"));
        let result = guard.validate_mount(Path::new("/home/user/.ssh"), true);
        assert!(result.is_err());
    }
}
