//! `secret://file/<absolute-path>` — file-mounted secrets (WP-H1 P1).
//!
//! The delivery format every container orchestrator already speaks: Docker
//! `secrets:` land in `/run/secrets/<name>`, Kubernetes projected volumes in
//! `/var/run/secrets/...`. The enterprise Docker / OEM line ships those mounts
//! today, so this adapter is what lets a packaged deployment keep credentials
//! out of `config.toml` entirely — the config holds
//! `secret://file//run/secrets/tg_token` (note the doubled slash: the backend
//! name is `file`, everything after it is an absolute path) and the value is
//! whatever the orchestrator injected.
//!
//! ## Containment
//!
//! An unrestricted "read this path" backend turns any write to `config.toml`
//! into an arbitrary-file-read primitive. So the path is confined to a fixed
//! root set — [`allowed_roots`] — and confinement is checked **after**
//! canonicalisation, which is what stops a symlink inside a legitimate root
//! from pointing at `/etc/shadow`.
//!
//! The roots are deliberately compile-time constants plus one home-relative
//! directory, with no config knob: the synchronous resolver
//! ([`crate::secret_ref::SecretRef::resolve_sync`]) has a `home_dir` and
//! nothing else, and a root list that existed only on the async path would
//! mean the same reference resolved differently depending on which call site
//! read it. One rule everywhere beats a configurable rule that drifts.
//!
//! ## Permissions
//!
//! World-**writable** is refused outright: anyone on the box could substitute a
//! credential, which is a real privilege boundary. World- or group-*readable*
//! only warns — Docker mounts secrets `0444` and Kubernetes projects them
//! `0644` by default, so refusing those would reject the exact deployments this
//! backend exists to serve. That is a deliberate softening of the design doc's
//! "0600 檢查"; it is recorded here rather than done silently.

use async_trait::async_trait;
use duduclaw_core::error::{DuDuClawError, Result};
use std::path::{Path, PathBuf};

use super::SecretManager;

/// Largest secret file this adapter will read. A mistyped path pointing at a
/// multi-gigabyte log must fail, not exhaust memory.
const MAX_SECRET_FILE_BYTES: u64 = 64 * 1024;

/// Directories a `secret://file/...` reference may name.
///
/// - `/run/secrets` — Docker Compose / Swarm secrets.
/// - `/var/run/secrets` — Kubernetes projected service-account & secret volumes.
/// - `<home>/secrets` — the operator's own drop directory, for bare-metal
///   installs that have no orchestrator.
pub fn allowed_roots(home_dir: &Path) -> Vec<PathBuf> {
    vec![
        PathBuf::from("/run/secrets"),
        PathBuf::from("/var/run/secrets"),
        home_dir.join("secrets"),
    ]
}

/// Resolve `name` to a real file inside an allowed root.
///
/// Every rejection is an `Err` carrying the reason but never the file contents.
fn vet_path(name: &str, home_dir: &Path) -> Result<PathBuf> {
    let raw = Path::new(name);
    if !raw.is_absolute() {
        return Err(DuDuClawError::Security(format!(
            "secret://file reference must be an absolute path (got '{name}')"
        )));
    }

    // Canonicalise first: this both resolves symlinks (so containment is
    // checked against the real target) and proves the file exists.
    let real = raw.canonicalize().map_err(|e| {
        DuDuClawError::Security(format!("secret://file path '{name}' is unreadable: {e}"))
    })?;

    let roots = allowed_roots(home_dir);
    let contained = roots.iter().any(|root| {
        // Compare canonicalised roots too — on macOS `/var` is itself a symlink
        // to `/private/var`, so an uncanonicalised prefix test would reject
        // every Kubernetes-style path on a developer machine.
        let root = root.canonicalize().unwrap_or_else(|_| root.clone());
        real.starts_with(&root)
    });
    if !contained {
        let root_list = roots
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(DuDuClawError::Security(format!(
            "secret://file path '{name}' resolves outside the permitted roots ({root_list})"
        )));
    }

    let meta = std::fs::metadata(&real).map_err(|e| {
        DuDuClawError::Security(format!("secret://file path '{name}' is unreadable: {e}"))
    })?;
    if !meta.is_file() {
        return Err(DuDuClawError::Security(format!(
            "secret://file path '{name}' is not a regular file"
        )));
    }
    if meta.len() > MAX_SECRET_FILE_BYTES {
        return Err(DuDuClawError::Security(format!(
            "secret://file path '{name}' is {} bytes — the maximum is {MAX_SECRET_FILE_BYTES}",
            meta.len()
        )));
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = meta.permissions().mode() & 0o777;
        if mode & 0o002 != 0 {
            return Err(DuDuClawError::Security(format!(
                "secret://file path '{name}' is world-writable (mode {mode:o}) — refusing to \
                 read a credential anyone on this host can replace"
            )));
        }
        if mode & 0o077 != 0 {
            tracing::warn!(
                path = %real.display(),
                mode = format!("{mode:o}"),
                "secret://file target is readable beyond its owner — expected for Docker/K8s \
                 mounts, worth tightening on a host you control"
            );
        }
    }

    Ok(real)
}

/// Reads secrets from files on disk, confined to [`allowed_roots`].
pub struct FileSecretAdapter {
    home_dir: PathBuf,
}

impl FileSecretAdapter {
    pub fn new(home_dir: impl Into<PathBuf>) -> Self {
        Self {
            home_dir: home_dir.into(),
        }
    }

    /// Blocking read used by both the async trait method and the synchronous
    /// resolver — file I/O is local, so there is nothing to gain from making
    /// two implementations that can disagree.
    pub fn read_blocking(&self, name: &str) -> Result<String> {
        let path = vet_path(name, &self.home_dir)?;
        let bytes = std::fs::read(&path).map_err(|e| {
            DuDuClawError::Security(format!("secret://file read of '{name}' failed: {e}"))
        })?;
        let text = String::from_utf8(bytes).map_err(|_| {
            DuDuClawError::Security(format!("secret://file '{name}' is not valid UTF-8"))
        })?;
        // One trailing newline is an artefact of how the file was written
        // (`echo` / a here-doc / an editor), not part of the credential.
        // Anything beyond that single newline is left alone — trimming freely
        // would silently alter a token whose whitespace is significant.
        let value = text
            .strip_suffix('\n')
            .map(|s| s.strip_suffix('\r').unwrap_or(s))
            .unwrap_or(&text);
        Ok(value.to_string())
    }
}

#[async_trait]
impl SecretManager for FileSecretAdapter {
    async fn get(&self, name: &str) -> Result<String> {
        self.read_blocking(name)
    }

    async fn put(&self, _name: &str, _value: &str) -> Result<()> {
        Err(DuDuClawError::Security(
            "secret://file is read-only — write the value with the orchestrator that mounts it"
                .to_string(),
        ))
    }

    async fn delete(&self, _name: &str) -> Result<()> {
        Err(DuDuClawError::Security(
            "secret://file is read-only — remove the mount with the orchestrator that owns it"
                .to_string(),
        ))
    }

    async fn exists(&self, name: &str) -> Result<bool> {
        Ok(vet_path(name, &self.home_dir).is_ok())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static N: AtomicU64 = AtomicU64::new(0);

    struct TempHome(PathBuf);
    impl TempHome {
        fn new() -> Self {
            let n = N.fetch_add(1, Ordering::Relaxed);
            let p = std::env::temp_dir()
                .join(format!("duduclaw-filesecret-{}-{n}", std::process::id()));
            std::fs::create_dir_all(p.join("secrets")).unwrap();
            Self(p)
        }
        fn write_secret(&self, name: &str, body: &str) -> PathBuf {
            let p = self.0.join("secrets").join(name);
            std::fs::write(&p, body).unwrap();
            p
        }
    }
    impl Drop for TempHome {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn reads_a_secret_and_strips_one_trailing_newline() {
        let home = TempHome::new();
        let p = home.write_secret("tok", "s3cr3t\n");
        let a = FileSecretAdapter::new(&home.0);
        assert_eq!(a.read_blocking(p.to_str().unwrap()).unwrap(), "s3cr3t");

        let p2 = home.write_secret("tok2", "s3cr3t\r\n");
        assert_eq!(a.read_blocking(p2.to_str().unwrap()).unwrap(), "s3cr3t");

        // Two newlines: only the last is an artefact, the rest is content.
        let p3 = home.write_secret("tok3", "a\n\n");
        assert_eq!(a.read_blocking(p3.to_str().unwrap()).unwrap(), "a\n");
    }

    #[test]
    fn refuses_a_relative_path() {
        let home = TempHome::new();
        let a = FileSecretAdapter::new(&home.0);
        assert!(a.read_blocking("secrets/tok").is_err());
    }

    #[test]
    fn refuses_a_path_outside_the_allowed_roots() {
        let home = TempHome::new();
        // A real, readable file that is nonetheless not in a secrets root.
        let outside = home.0.join("not-a-secret");
        std::fs::write(&outside, "nope").unwrap();
        let a = FileSecretAdapter::new(&home.0);
        let err = a.read_blocking(outside.to_str().unwrap()).unwrap_err();
        assert!(
            err.to_string().contains("outside the permitted roots"),
            "{err}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn refuses_a_symlink_that_escapes_the_root() {
        let home = TempHome::new();
        let outside = home.0.join("escape-target");
        std::fs::write(&outside, "pwned").unwrap();
        let link = home.0.join("secrets").join("innocent");
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        let a = FileSecretAdapter::new(&home.0);
        assert!(
            a.read_blocking(link.to_str().unwrap()).is_err(),
            "containment must be checked after canonicalisation"
        );
    }

    #[cfg(unix)]
    #[test]
    fn refuses_a_world_writable_secret_but_tolerates_world_readable() {
        use std::os::unix::fs::PermissionsExt;
        let home = TempHome::new();
        let a = FileSecretAdapter::new(&home.0);

        let bad = home.write_secret("loose", "v");
        std::fs::set_permissions(&bad, std::fs::Permissions::from_mode(0o666)).unwrap();
        assert!(a.read_blocking(bad.to_str().unwrap()).is_err());

        // 0444 is exactly how Docker mounts a secret.
        let ok = home.write_secret("docker-style", "v");
        std::fs::set_permissions(&ok, std::fs::Permissions::from_mode(0o444)).unwrap();
        assert_eq!(a.read_blocking(ok.to_str().unwrap()).unwrap(), "v");
    }

    #[test]
    fn refuses_a_directory_and_a_missing_file() {
        let home = TempHome::new();
        let a = FileSecretAdapter::new(&home.0);
        let dir = home.0.join("secrets");
        assert!(a.read_blocking(dir.to_str().unwrap()).is_err());
        assert!(
            a.read_blocking(home.0.join("secrets/absent").to_str().unwrap())
                .is_err()
        );
    }

    #[test]
    fn refuses_an_oversized_file() {
        let home = TempHome::new();
        let big = "x".repeat((MAX_SECRET_FILE_BYTES + 1) as usize);
        let p = home.write_secret("big", &big);
        let a = FileSecretAdapter::new(&home.0);
        assert!(a.read_blocking(p.to_str().unwrap()).is_err());
    }

    #[tokio::test]
    async fn writes_are_refused() {
        let home = TempHome::new();
        let a = FileSecretAdapter::new(&home.0);
        assert!(a.put("x", "y").await.is_err());
        assert!(a.delete("x").await.is_err());
    }
}
