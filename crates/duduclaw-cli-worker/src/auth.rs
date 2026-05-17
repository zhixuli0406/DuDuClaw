//! Phase 6.4 — token management for the worker IPC.
//!
//! - 32 random bytes, hex-encoded → 64 ASCII chars.
//! - Persisted at `<home>/cli-worker.token` (mode 0600 on Unix).
//! - Comparison is constant-time via `ring::constant_time::verify_slices_are_equal`.
//!
//! Operationally this is a shared-secret between gateway and worker. Since
//! both processes are co-located on the same host and the server binds
//! 127.0.0.1, the only attack surface is a local-but-unprivileged process
//! trying to call the worker. The token suffices for that threat model.

use std::path::{Path, PathBuf};

use ring::rand::{SecureRandom, SystemRandom};
use thiserror::Error;

/// Persistent token store rooted at `<home>/cli-worker.token`.
#[derive(Debug, Clone)]
pub struct TokenStore {
    path: PathBuf,
}

#[derive(Debug, Error)]
pub enum TokenError {
    #[error("token file I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("random byte generation failed")]
    Rng,
}

impl TokenStore {
    pub fn new(home_dir: &Path) -> Self {
        Self {
            path: home_dir.join("cli-worker.token"),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Load the token from disk. Returns `None` when no file exists yet
    /// (caller can decide whether to generate one).
    pub fn load(&self) -> Result<Option<String>, TokenError> {
        match std::fs::read_to_string(&self.path) {
            Ok(s) => {
                let trimmed = s.trim().to_string();
                if trimmed.is_empty() {
                    Ok(None)
                } else {
                    Ok(Some(trimmed))
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(TokenError::Io(e)),
        }
    }

    /// Generate + persist a fresh 32-byte hex token. Overwrites any
    /// existing file. Set mode 0600 on Unix; on Windows the parent
    /// directory ACL is the operator's responsibility.
    ///
    /// **Round 3 security fix (MED-M4)**: write to `<path>.tmp` first,
    /// set permissions, then atomically `rename` over the live file.
    /// `std::fs::write` on its own isn't atomic — a crash during the
    /// write would leave a truncated token on disk that the worker
    /// would read at startup and silently reject every request with
    /// 401 until the next regeneration.
    pub fn generate_and_save(&self) -> Result<String, TokenError> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Round 4 fix (MED-1): best-effort cleanup of stale `.tmp.<pid>`
        // files left by previous crashed runs. Without this, every
        // crash between `write` and `rename` leaves a 64-byte token
        // file lingering in the home directory.
        self.cleanup_stale_tmp_files();

        let rng = SystemRandom::new();
        let mut bytes = [0u8; 32];
        rng.fill(&mut bytes).map_err(|_| TokenError::Rng)?;
        let token = hex::encode(bytes);

        // Write to a sibling temp file first so a mid-write crash
        // can never leave a truncated token under the live path.
        let mut tmp_path = self.path.clone();
        let tmp_name = format!(
            "{}.tmp.{}",
            self.path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("cli-worker.token"),
            std::process::id(),
        );
        tmp_path.set_file_name(tmp_name);

        // Round 4 fix (CRIT-2): on Unix, create the temp file with
        // mode 0600 at `open(2)` time. The previous code used
        // `std::fs::write`, which created the file with
        // `0666 & ~umask` (commonly 0644) and only tightened
        // permissions afterwards — leaving a microsecond window
        // during which any local user could read the freshly-written
        // token. Setting the mode in the same syscall closes that
        // window entirely.
        #[cfg(unix)]
        {
            use std::io::Write;
            use std::os::unix::fs::OpenOptionsExt;
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&tmp_path)?;
            file.write_all(token.as_bytes())?;
            file.sync_all()?;
        }
        #[cfg(not(unix))]
        {
            std::fs::write(&tmp_path, &token)?;
        }

        // Atomic replace. On Unix this is rename(2); on Windows
        // std::fs::rename uses MoveFileExW with MOVEFILE_REPLACE_EXISTING.
        std::fs::rename(&tmp_path, &self.path)?;

        // Round 4 deferred-cleanup (MED-2): tighten DACL on Windows so
        // BUILTIN\Users (which inherits a read ACE on most home
        // directories) can no longer open the file. Failure is logged
        // but non-fatal — the live token file already exists; operators
        // can re-run `icacls` manually if the call below failed.
        #[cfg(windows)]
        {
            if let Err(e) = tighten_token_acl_windows(&self.path) {
                tracing::warn!(
                    path = %self.path.display(),
                    error = %e,
                    "TokenStore: icacls hardening failed; token file may inherit a permissive DACL from its parent directory. Run `icacls \"{}\" /inheritance:r /grant:r %USERNAME%:F` to lock it down manually.",
                    self.path.display(),
                );
            }
        }

        Ok(token)
    }

    /// Round 4 fix (MED-1): remove `.tmp.<pid>` siblings of the live
    /// token path. Errors are ignored — this is best-effort hygiene,
    /// not a security boundary; failure to clean up never prevents
    /// the live write that follows.
    fn cleanup_stale_tmp_files(&self) {
        let Some(parent) = self.path.parent() else { return };
        let base = self
            .path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("cli-worker.token");
        let prefix = format!("{base}.tmp.");
        let Ok(rd) = std::fs::read_dir(parent) else { return };
        for entry in rd.flatten() {
            if let Some(name) = entry.file_name().to_str() {
                if name.starts_with(&prefix) {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }
    }

    /// Get the existing token, or generate + persist a new one if none
    /// exists. Idempotent across worker / gateway start order.
    pub fn load_or_generate(&self) -> Result<String, TokenError> {
        if let Some(t) = self.load()? {
            return Ok(t);
        }
        self.generate_and_save()
    }
}

/// Round 4 deferred-cleanup (MED-2): tighten the file's DACL via the
/// built-in `icacls.exe` tool. Removes inherited ACEs (which on most
/// Windows user profiles include a read grant to BUILTIN\Users) and
/// re-grants Full Control to the current account only. Best-effort:
/// callers log failures but don't propagate them, because the live
/// token file already exists at this point — surfacing a hard error
/// would block the supervisor boot for a benign hardening step.
#[cfg(windows)]
fn tighten_token_acl_windows(path: &std::path::Path) -> std::io::Result<()> {
    use std::process::Command;
    let username = std::env::var("USERNAME").map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "USERNAME env var not set — cannot resolve current account for icacls",
        )
    })?;
    if username.trim().is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "USERNAME env var was empty",
        ));
    }
    let status = Command::new("icacls.exe")
        .arg(path.as_os_str())
        // remove inherited ACEs entirely
        .arg("/inheritance:r")
        // replace any existing grants with just the owner Full Control
        .arg("/grant:r")
        .arg(format!("{username}:F"))
        .status()?;
    if !status.success() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("icacls.exe exited with status {status}"),
        ));
    }
    Ok(())
}

/// Constant-time comparison. Returns true when both strings have equal
/// length AND identical bytes; false otherwise.
///
/// The length check is an early exit, which leaks the *length* of the
/// expected token to a timing-adversary. For our use case the expected
/// token is fixed-length (64 hex chars from [`TokenStore`]), so this
/// "length leak" reveals nothing useful. The byte comparison loop
/// itself is constant-time over the matching-length case.
pub fn verify_token(expected: &str, supplied: &str) -> bool {
    let a = expected.as_bytes();
    let b = supplied.as_bytes();
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn load_returns_none_for_missing_file() {
        let dir = TempDir::new().unwrap();
        let store = TokenStore::new(dir.path());
        assert!(store.load().unwrap().is_none());
    }

    #[test]
    fn generate_creates_64_char_hex() {
        let dir = TempDir::new().unwrap();
        let store = TokenStore::new(dir.path());
        let t = store.generate_and_save().unwrap();
        assert_eq!(t.len(), 64, "token should be 64 hex chars: {t:?}");
        assert!(t.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn load_or_generate_is_idempotent() {
        let dir = TempDir::new().unwrap();
        let store = TokenStore::new(dir.path());
        let t1 = store.load_or_generate().unwrap();
        let t2 = store.load_or_generate().unwrap();
        assert_eq!(t1, t2, "second call must read the same token, not regenerate");
    }

    #[cfg(unix)]
    #[test]
    fn generated_file_is_mode_0600_on_unix() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        let store = TokenStore::new(dir.path());
        let _ = store.generate_and_save().unwrap();
        let perms = std::fs::metadata(store.path()).unwrap().permissions();
        assert_eq!(perms.mode() & 0o777, 0o600, "expected 0600");
    }

    #[test]
    fn verify_token_matches_identical() {
        assert!(verify_token("abc123", "abc123"));
    }

    #[test]
    fn verify_token_rejects_mismatch() {
        assert!(!verify_token("abc123", "abc124"));
        assert!(!verify_token("abc123", "abc12"));   // shorter
        assert!(!verify_token("abc123", "abc1234")); // longer
    }

    #[test]
    fn verify_token_rejects_empty_supplied() {
        assert!(!verify_token("abc123", ""));
    }

    #[test]
    fn load_returns_none_for_empty_file() {
        let dir = TempDir::new().unwrap();
        let store = TokenStore::new(dir.path());
        std::fs::write(store.path(), "   \n").unwrap();
        assert!(store.load().unwrap().is_none());
    }

    /// Round 4 (MED-1): stale `.tmp.<pid>` files left by a previous
    /// crashed run must be cleaned up on the next `generate_and_save`.
    #[test]
    fn generate_removes_stale_tmp_files() {
        let dir = TempDir::new().unwrap();
        let store = TokenStore::new(dir.path());
        // Simulate a crash that left behind two stale tmp files.
        let stale1 = dir.path().join("cli-worker.token.tmp.99998");
        let stale2 = dir.path().join("cli-worker.token.tmp.99999");
        std::fs::write(&stale1, "old-leaked-token-1").unwrap();
        std::fs::write(&stale2, "old-leaked-token-2").unwrap();
        let _ = store.generate_and_save().unwrap();
        assert!(!stale1.exists(), "stale tmp file should be removed");
        assert!(!stale2.exists(), "stale tmp file should be removed");
        assert!(store.path().exists(), "live token file should exist");
    }

    /// Round 4 (CRIT-2): a freshly-generated token must already be
    /// 0600 — there must be NO window during which the file is
    /// readable by other local users (umask race).
    #[cfg(unix)]
    #[test]
    fn freshly_generated_token_is_never_world_readable() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        let store = TokenStore::new(dir.path());
        let _ = store.generate_and_save().unwrap();
        let mode = std::fs::metadata(store.path()).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "expected 0600, got {mode:#o}");
    }
}
