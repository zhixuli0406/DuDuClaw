//! Host checkpoint manifests and descriptor-only delivery. Publication happens
//! in the existing checkpoint exporter; protected reads never walk an output
//! tree or interpret a pathname supplied by an employee as a host root.
use std::{collections::BTreeMap, path::{Path, PathBuf}};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const MAX_ENTRIES: usize = 4096;
pub const MAX_MANIFEST_BYTES: usize = 1024 * 1024;
// Base64 plus both JSON envelopes must stay below the 4-MiB MCP boundary.
pub const MAX_DOWNLOAD: u64 = 2 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactManifest {
    version: u16,
    hashes: BTreeMap<String, String>,
}

impl ArtifactManifest {
    /// The checkpoint exporter owns these hashes. Never construct authority by
    /// loading a manifest from the employee workspace or a tool argument.
    pub fn from_checkpoint(hashes: BTreeMap<PathBuf, String>) -> Result<Self, String> {
        let hashes = hashes.into_iter().map(|(path, digest)| {
            Ok((path.into_os_string().into_string().map_err(|_| "non-UTF-8 artifact name")?, digest))
        }).collect::<Result<BTreeMap<_, _>, String>>()?;
        let manifest = Self { version: 1, hashes };
        manifest.validate()?;
        Ok(manifest)
    }

    fn validate(&self) -> Result<(), String> {
        if self.version != 1 || self.hashes.len() > MAX_ENTRIES {
            return Err("artifact manifest exceeds bounded size".into());
        }
        for (name, digest) in &self.hashes {
            duduclaw_mcp_boundary::resources::relative_path(name).map_err(|_| "invalid artifact name")?;
            if name.chars().any(char::is_control) || (digest != "directory" && !valid_digest(digest)) {
                return Err("invalid artifact manifest".into());
            }
            // Require every ancestor's directory entry, preventing ambiguous
            // manifests such as a file entry serving as another file's parent.
            let mut parent = Path::new(name).parent();
            while let Some(path) = parent.filter(|path| !path.as_os_str().is_empty()) {
                if self.hashes.get(path.to_str().ok_or("invalid artifact name")?).map(String::as_str) != Some("directory") {
                    return Err("incomplete artifact manifest".into());
                }
                parent = path.parent();
            }
        }
        Ok(())
    }

    pub fn encode(&self) -> Result<String, String> {
        self.validate()?;
        let encoded = serde_json::to_string(self).map_err(|_| "artifact manifest encoding failed")?;
        if encoded.len() > MAX_MANIFEST_BYTES { return Err("artifact manifest exceeds bounded size".into()); }
        Ok(encoded)
    }

    pub fn decode(encoded: &str, expected: &str) -> Result<Self, String> {
        if encoded.len() > MAX_MANIFEST_BYTES || !valid_digest(expected) {
            return Err("invalid artifact manifest".into());
        }
        let manifest: Self = serde_json::from_str(encoded).map_err(|_| "invalid artifact manifest")?;
        manifest.validate()?;
        if manifest.digest() != expected { return Err("artifact checkpoint manifest changed".into()); }
        Ok(manifest)
    }

    pub fn digest(&self) -> String {
        let mut digest = Sha256::new();
        for (name, hash) in &self.hashes {
            digest.update((name.len() as u64).to_be_bytes());
            digest.update(name.as_bytes());
            digest.update(hash.as_bytes());
        }
        format!("{:x}", digest.finalize())
    }

    pub fn files(&self) -> impl Iterator<Item = (&str, &str)> {
        self.hashes.iter().filter(|(_, hash)| hash.as_str() != "directory")
            .map(|(name, hash)| (name.as_str(), hash.as_str()))
    }
}

fn valid_digest(digest: &str) -> bool {
    digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// The opaque input is produced only after the caller/task/run ACL and durable
/// checkpoint binding have been checked. No raw dirfd or host path is exposed
/// through the public transport.
#[cfg(target_os = "linux")]
pub struct AuthorizedArtifact { root: std::fs::File }

#[cfg(target_os = "linux")]
impl AuthorizedArtifact {
    pub(super) fn open(home: &Path, run: &str, cell: &str) -> Result<Self, String> {
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
        if !safe_id(run) || !safe_id(cell) { return Err("invalid artifact identity".into()); }
        let root = std::fs::OpenOptions::new().read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(home).map_err(|_| "artifact storage unavailable")?;
        let device = root.metadata().map_err(|_| "artifact storage unavailable")?.dev();
        check_host_directory(&root, device)?;
        let mut root = root;
        for component in ["discovery", "artifacts", run, cell, "ws"] {
            root = open_component(&root, component, true)?;
            check_host_directory(&root, device)?;
        }
        Ok(Self { root })
    }

    pub fn open_file(&self, name: &str) -> Result<std::fs::File, String> {
        use std::os::unix::fs::MetadataExt;
        let path = duduclaw_mcp_boundary::resources::relative_path(name).map_err(|_| "invalid artifact name")?;
        let device = self.root.metadata().map_err(|_| "artifact storage unavailable")?.dev();
        let mut directory = self.root.try_clone().map_err(|_| "artifact storage unavailable")?;
        let mut components = path.components().peekable();
        while let Some(component) = components.next() {
            let name = component.as_os_str().to_str().ok_or("invalid artifact name")?;
            let is_directory = components.peek().is_some();
            let file = open_component(&directory, name, is_directory)?;
            if is_directory {
                check_host_directory(&file, device)?;
                directory = file;
            } else {
                let meta = file.metadata().map_err(|_| "artifact file unavailable")?;
                // Same-mount, single-link, host-owned export only. NONBLOCK
                // avoids FIFO blocking before the regular-file check.
                if !meta.is_file() || meta.dev() != device || meta.nlink() != 1
                    || meta.uid() != nix::unistd::geteuid().as_raw() || meta.mode() & 0o022 != 0 {
                    return Err("artifact file binding changed".into());
                }
                return Ok(file);
            }
        }
        Err("artifact file not found".into())
    }
}

#[cfg(target_os = "linux")]
fn safe_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 128 && id.bytes().all(|byte|
        byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

#[cfg(target_os = "linux")]
fn check_host_directory(file: &std::fs::File, device: u64) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;
    let meta = file.metadata().map_err(|_| "artifact storage unavailable")?;
    if !meta.is_dir() || meta.dev() != device || meta.uid() != nix::unistd::geteuid().as_raw()
        || meta.mode() & 0o022 != 0 {
        return Err("artifact storage binding changed".into());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn open_component(directory: &std::fs::File, name: &str, is_directory: bool) -> Result<std::fs::File, String> {
    use std::os::fd::{AsRawFd, FromRawFd};
    let name = std::ffi::CString::new(name).map_err(|_| "invalid artifact name")?;
    let flags = libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_NOCTTY
        | if is_directory { libc::O_DIRECTORY } else { 0 };
    #[repr(C)]
    struct OpenHow { flags: u64, mode: u64, resolve: u64 }
    // Linux openat2 UAPI: BENEATH | NO_XDEV | NO_MAGICLINKS | NO_SYMLINKS.
    // In particular, device equality alone would not reject bind submounts.
    let how = OpenHow { flags: flags as u64, mode: 0, resolve: 0x08 | 0x01 | 0x02 | 0x04 };
    // SAFETY: directory is retained, name is NUL-terminated, and the repr(C)
    // open_how value has the exact Linux ABI layout and explicit size.
    let fd = unsafe { libc::syscall(libc::SYS_openat2, directory.as_raw_fd(), name.as_ptr(),
        &how, std::mem::size_of::<OpenHow>()) };
    if fd < 0 { return Err("artifact file unavailable".into()); }
    // SAFETY: openat2 returned a fresh owned descriptor; Linux fds fit i32.
    Ok(unsafe { std::fs::File::from_raw_fd(fd as i32) })
}

#[cfg(test)]
#[path = "tests_artifact_manifest.rs"]
mod tests;
