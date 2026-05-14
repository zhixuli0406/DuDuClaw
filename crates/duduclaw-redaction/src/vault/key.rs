//! Per-agent vault encryption key — generated on first use, persisted to
//! `<key_dir>/<agent_id>.key` with mode `0o600`.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use ring::rand::{SecureRandom, SystemRandom};

use crate::error::{RedactionError, Result};

/// Raw key length (32 bytes for AES-256-GCM).
pub const KEY_LEN: usize = 32;

/// Load (or generate-on-miss) the redaction key for a given agent.
///
/// - On first call: generates a fresh 32-byte key, writes it to
///   `<key_dir>/<agent_id>.key` with `0o600` permissions (Unix).
/// - On subsequent calls: reads the existing key.
/// - Treats file shorter than 32 bytes as corrupt.
pub fn load_or_generate(agent_id: &str, key_dir: &Path) -> Result<[u8; KEY_LEN]> {
    if !is_safe_agent_id(agent_id) {
        return Err(RedactionError::crypto(format!(
            "agent_id contains unsafe characters: {agent_id}"
        )));
    }

    fs::create_dir_all(key_dir)?;
    let path = key_path(agent_id, key_dir);

    if path.exists() {
        let bytes = fs::read(&path)?;
        if bytes.len() != KEY_LEN {
            return Err(RedactionError::crypto(format!(
                "key file {} has invalid length {}",
                path.display(),
                bytes.len()
            )));
        }
        let mut key = [0u8; KEY_LEN];
        key.copy_from_slice(&bytes);
        return Ok(key);
    }

    // Generate fresh key.
    let mut key = [0u8; KEY_LEN];
    SystemRandom::new()
        .fill(&mut key)
        .map_err(|e| RedactionError::crypto(format!("RNG failure: {e}")))?;

    write_key_secure(&path, &key)?;
    Ok(key)
}

/// Compute the key file path for an agent.
pub fn key_path(agent_id: &str, key_dir: &Path) -> PathBuf {
    key_dir.join(format!("{agent_id}.key"))
}

fn is_safe_agent_id(agent_id: &str) -> bool {
    !agent_id.is_empty()
        && agent_id.len() <= 128
        && agent_id
            .bytes()
            .all(|b| matches!(b, b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_'))
}

#[cfg(unix)]
fn write_key_secure(path: &Path, key: &[u8]) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;

    let mut f = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(key)?;
    f.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
fn write_key_secure(path: &Path, key: &[u8]) -> Result<()> {
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    f.write_all(key)?;
    f.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn generates_then_reads_back() {
        let tmp = TempDir::new().unwrap();
        let k1 = load_or_generate("agnes", tmp.path()).unwrap();
        let k2 = load_or_generate("agnes", tmp.path()).unwrap();
        assert_eq!(k1, k2);
    }

    #[test]
    fn different_agents_get_different_keys() {
        let tmp = TempDir::new().unwrap();
        let k1 = load_or_generate("agnes", tmp.path()).unwrap();
        let k2 = load_or_generate("bobby", tmp.path()).unwrap();
        assert_ne!(k1, k2);
    }

    #[test]
    fn unsafe_agent_id_rejected() {
        let tmp = TempDir::new().unwrap();
        assert!(load_or_generate("../etc/passwd", tmp.path()).is_err());
        assert!(load_or_generate("", tmp.path()).is_err());
        assert!(load_or_generate("agent with space", tmp.path()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn key_file_has_0600_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = TempDir::new().unwrap();
        load_or_generate("agnes", tmp.path()).unwrap();
        let meta = fs::metadata(key_path("agnes", tmp.path())).unwrap();
        let mode = meta.permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}
