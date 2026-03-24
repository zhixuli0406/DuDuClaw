//! SOUL.md integrity protection — drift detection and version history.
//!
//! [C-1a] Computes SHA-256 fingerprint at startup and on each heartbeat tick.
//! [C-1b] Maintains up to 10 versioned backups in `.soul_history/`.

use std::path::{Path, PathBuf};

use ring::digest;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

const MAX_HISTORY_VERSIONS: usize = 10;

/// Result of a SOUL.md integrity check.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SoulCheckResult {
    pub agent_id: String,
    pub intact: bool,
    pub current_hash: String,
    pub expected_hash: String,
    pub message: String,
}

/// Compute the SHA-256 hex digest of a byte slice.
fn sha256_hex(data: &[u8]) -> String {
    let digest = digest::digest(&digest::SHA256, data);
    digest
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Compute the SHA-256 fingerprint of a SOUL.md file.
/// Returns `None` if the file does not exist.
pub fn fingerprint_soul(agent_dir: &Path) -> Option<String> {
    let soul_path = agent_dir.join("SOUL.md");
    let content = std::fs::read(&soul_path).ok()?;
    Some(sha256_hex(&content))
}

/// Read the stored fingerprint from `.soul_hash`.
pub fn read_stored_hash(agent_dir: &Path) -> Option<String> {
    let hash_path = agent_dir.join(".soul_hash");
    std::fs::read_to_string(&hash_path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Persist the fingerprint to `.soul_hash`.
pub fn store_hash(agent_dir: &Path, hash: &str) -> std::io::Result<()> {
    let hash_path = agent_dir.join(".soul_hash");
    std::fs::write(&hash_path, hash)
}

/// Check SOUL.md integrity for a single agent.
///
/// - If no stored hash exists, computes and stores the initial fingerprint.
/// - If the hash matches, returns `intact = true`.
/// - If the hash differs, returns `intact = false` with details.
pub fn check_soul_integrity(agent_id: &str, agent_dir: &Path) -> SoulCheckResult {
    let current = match fingerprint_soul(agent_dir) {
        Some(h) => h,
        None => {
            return SoulCheckResult {
                agent_id: agent_id.to_string(),
                intact: true,
                current_hash: String::new(),
                expected_hash: String::new(),
                message: "No SOUL.md file (optional)".to_string(),
            };
        }
    };

    let stored = read_stored_hash(agent_dir);

    match stored {
        Some(expected) if expected == current => SoulCheckResult {
            agent_id: agent_id.to_string(),
            intact: true,
            current_hash: current,
            expected_hash: expected,
            message: "SOUL.md integrity verified".to_string(),
        },
        Some(expected) => {
            warn!(
                agent = agent_id,
                expected = %expected,
                current = %current,
                "SOUL.md drift detected!"
            );
            let msg = format!(
                "SOUL.md content changed! Expected hash: {expected}, got: {current}"
            );
            SoulCheckResult {
                agent_id: agent_id.to_string(),
                intact: false,
                current_hash: current,
                expected_hash: expected,
                message: msg,
            }
        }
        None => {
            // First run — store initial fingerprint
            if let Err(e) = store_hash(agent_dir, &current) {
                warn!(agent = agent_id, "Failed to store initial SOUL hash: {e}");
            } else {
                info!(agent = agent_id, hash = %current, "SOUL.md fingerprint initialized");
            }
            SoulCheckResult {
                agent_id: agent_id.to_string(),
                intact: true,
                current_hash: current.clone(),
                expected_hash: current,
                message: "SOUL.md fingerprint initialized (first run)".to_string(),
            }
        }
    }
}

/// Accept a SOUL.md change: update the stored hash and save a backup.
///
/// Call this after a legitimate SOUL.md modification (e.g., evolution update).
pub fn accept_soul_change(agent_id: &str, agent_dir: &Path) -> std::io::Result<()> {
    let current = match fingerprint_soul(agent_dir) {
        Some(h) => h,
        None => return Ok(()),
    };

    // Save version history before updating hash
    save_soul_version(agent_dir)?;

    store_hash(agent_dir, &current)?;
    info!(agent = agent_id, hash = %current, "SOUL.md change accepted, hash updated");
    Ok(())
}

// ── Version history (C-1b) ──────────────────────────────────

/// Save the current SOUL.md content to `.soul_history/SOUL_<timestamp>.md`.
/// Keeps at most `MAX_HISTORY_VERSIONS` backups.
fn save_soul_version(agent_dir: &Path) -> std::io::Result<()> {
    let soul_path = agent_dir.join("SOUL.md");
    let content = match std::fs::read(&soul_path) {
        Ok(c) => c,
        Err(_) => return Ok(()), // no SOUL.md to back up
    };

    let history_dir = agent_dir.join(".soul_history");
    std::fs::create_dir_all(&history_dir)?;

    let timestamp = chrono::Utc::now().format("%Y%m%d_%H%M%S_%3f");
    let backup_name = format!("SOUL_{timestamp}.md");
    std::fs::write(history_dir.join(&backup_name), &content)?;

    // Prune old versions
    prune_history(&history_dir)?;

    info!(backup = %backup_name, "SOUL.md version saved");
    Ok(())
}

/// Remove oldest backups if more than `MAX_HISTORY_VERSIONS` exist.
fn prune_history(history_dir: &Path) -> std::io::Result<()> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(history_dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext == "md")
        })
        .collect();

    // Sort ascending by filename (timestamp-based)
    entries.sort();

    while entries.len() > MAX_HISTORY_VERSIONS {
        if let Some(oldest) = entries.first() {
            std::fs::remove_file(oldest)?;
            entries.remove(0);
        }
    }

    Ok(())
}

/// List all SOUL.md version history files for an agent.
pub fn list_soul_history(agent_dir: &Path) -> Vec<String> {
    let history_dir = agent_dir.join(".soul_history");
    let mut entries: Vec<String> = std::fs::read_dir(&history_dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            e.file_name()
                .to_str()
                .filter(|n| n.ends_with(".md"))
                .map(|n| n.to_string())
        })
        .collect();
    entries.sort();
    entries
}
