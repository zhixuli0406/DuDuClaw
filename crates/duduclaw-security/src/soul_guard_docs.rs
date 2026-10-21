//! Baselines for every prompt-bearing profile file, not only SOUL.md.
//!
//! `INSTRUCTIONS.md` is injected into every prompt like `SOUL.md`, so it gets
//! the same protection: a SHA-256 baseline outside the agent directory
//! (`<home>/soul_hashes/<agent>.instructions.hash`), an "established" marker
//! that makes a deleted hash suspicious (HS11), drift detection, and an
//! explicit accept after a reviewed change. SOUL.md keeps its original paths
//! and its `.soul_history/` backups; every SOUL call here delegates to the
//! existing functions in the parent module.

use std::path::Path;

use tracing::{info, warn};

/// A profile file whose content is protected by a stored baseline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtectedDoc {
    Soul,
    Instructions,
}

impl ProtectedDoc {
    /// File name inside the agent directory.
    pub fn file_name(self) -> &'static str {
        match self {
            Self::Soul => "SOUL.md",
            Self::Instructions => "INSTRUCTIONS.md",
        }
    }

    fn hash_ext(self) -> &'static str {
        match self {
            Self::Soul => "hash",
            Self::Instructions => "instructions.hash",
        }
    }

    fn marker_ext(self) -> &'static str {
        match self {
            Self::Soul => "established",
            Self::Instructions => "instructions.established",
        }
    }
}

/// How the file compares with its accepted baseline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocDrift {
    /// Matches the accepted baseline.
    Intact,
    /// Differs from the baseline, or the baseline hash is missing although
    /// one was established before.
    Drifted,
    /// No file, or no baseline was ever established.
    None,
}

impl DocDrift {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Intact => "intact",
            Self::Drifted => "drifted",
            Self::None => "none",
        }
    }
}

/// Result of [`check_doc_integrity`].
#[derive(Debug, Clone)]
pub struct DocCheckResult {
    pub intact: bool,
    pub current_hash: String,
    pub expected_hash: String,
    pub message: String,
}

/// Hex SHA-256 of `data` (the same digest the baselines store).
pub fn hash_bytes(data: &[u8]) -> String {
    super::sha256_hex(data)
}

/// SHA-256 of the file, `None` when it does not exist.
pub fn fingerprint_doc(doc: ProtectedDoc, agent_dir: &Path) -> Option<String> {
    match doc {
        ProtectedDoc::Soul => super::fingerprint_soul(agent_dir),
        ProtectedDoc::Instructions => {
            let content = std::fs::read(agent_dir.join(doc.file_name())).ok()?;
            Some(super::sha256_hex(&content))
        }
    }
}

/// The stored baseline hash, if any.
pub fn read_doc_hash(doc: ProtectedDoc, agent_dir: &Path) -> Option<String> {
    match doc {
        ProtectedDoc::Soul => super::read_stored_hash(agent_dir),
        ProtectedDoc::Instructions => {
            std::fs::read_to_string(super::hash_path_with(agent_dir, doc.hash_ext()))
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        }
    }
}

fn marker_path(doc: ProtectedDoc, agent_dir: &Path) -> std::path::PathBuf {
    super::hash_path_with(agent_dir, doc.marker_ext())
}

fn established(doc: ProtectedDoc, agent_dir: &Path) -> bool {
    marker_path(doc, agent_dir).exists()
}

fn store_doc_hash(doc: ProtectedDoc, agent_dir: &Path, hash: &str) -> std::io::Result<()> {
    match doc {
        ProtectedDoc::Soul => super::store_hash(agent_dir, hash),
        ProtectedDoc::Instructions => {
            std::fs::write(super::hash_path_with(agent_dir, doc.hash_ext()), hash)
        }
    }
}

fn mark_established(doc: ProtectedDoc, agent_dir: &Path) {
    let _ = std::fs::write(marker_path(doc, agent_dir), chrono::Utc::now().to_rfc3339());
}

/// Read-only drift state (never establishes a baseline).
pub fn doc_drift(doc: ProtectedDoc, agent_dir: &Path) -> DocDrift {
    let Some(current) = fingerprint_doc(doc, agent_dir) else {
        return DocDrift::None;
    };
    match read_doc_hash(doc, agent_dir) {
        Some(stored) if stored == current => DocDrift::Intact,
        Some(_) => DocDrift::Drifted,
        None if established(doc, agent_dir) => DocDrift::Drifted,
        None => DocDrift::None,
    }
}

/// Whether content hashing to `content_hash` is the accepted state: it
/// matches the stored baseline, or no baseline was ever established (the
/// next accept or heartbeat establishes one). A missing hash after an
/// established baseline is NOT accepted (HS11).
pub fn doc_baseline_matches(doc: ProtectedDoc, agent_dir: &Path, content_hash: &str) -> bool {
    match read_doc_hash(doc, agent_dir) {
        Some(stored) => stored == content_hash,
        None => !established(doc, agent_dir),
    }
}

/// Accept the file's current content as the new baseline.
pub fn accept_doc_change(doc: ProtectedDoc, agent_id: &str, agent_dir: &Path) -> std::io::Result<()> {
    if doc == ProtectedDoc::Soul {
        return super::accept_soul_change(agent_id, agent_dir);
    }
    let Some(current) = fingerprint_doc(doc, agent_dir) else {
        return Ok(());
    };
    store_doc_hash(doc, agent_dir, &current)?;
    mark_established(doc, agent_dir);
    info!(agent = agent_id, file = doc.file_name(), hash = %current, "profile file change accepted, baseline updated");
    Ok(())
}

/// After a derived write (an automatic rename), move the baseline along only
/// when the content BEFORE the write (`pre_hash`) was the accepted one. A
/// file that had already drifted stays drifted. Returns whether it accepted.
pub fn accept_if_was_intact(
    doc: ProtectedDoc,
    agent_id: &str,
    agent_dir: &Path,
    pre_hash: &str,
) -> std::io::Result<bool> {
    if !doc_baseline_matches(doc, agent_dir, pre_hash) {
        warn!(
            agent = agent_id,
            file = doc.file_name(),
            "profile file was already different from its approved version; baseline left as it was"
        );
        return Ok(false);
    }
    accept_doc_change(doc, agent_id, agent_dir)?;
    Ok(true)
}

/// Heartbeat-style integrity check: establishes the baseline on a genuine
/// first run, reports drift otherwise. SOUL delegates to
/// [`super::check_soul_integrity`] (content scan + ASI included).
pub fn check_doc_integrity(doc: ProtectedDoc, agent_id: &str, agent_dir: &Path) -> DocCheckResult {
    if doc == ProtectedDoc::Soul {
        let r = super::check_soul_integrity(agent_id, agent_dir);
        return DocCheckResult {
            intact: r.intact,
            current_hash: r.current_hash,
            expected_hash: r.expected_hash,
            message: r.message,
        };
    }
    let name = doc.file_name();
    let Some(current) = fingerprint_doc(doc, agent_dir) else {
        return DocCheckResult {
            intact: true,
            current_hash: String::new(),
            expected_hash: String::new(),
            message: format!("No {name} file (optional)"),
        };
    };
    match read_doc_hash(doc, agent_dir) {
        Some(expected) if expected == current => DocCheckResult {
            intact: true,
            current_hash: current,
            expected_hash: expected,
            message: format!("{name} integrity verified"),
        },
        Some(expected) => DocCheckResult {
            intact: false,
            message: format!("{name} content changed! Expected hash: {expected}, got: {current}"),
            current_hash: current,
            expected_hash: expected,
        },
        None if established(doc, agent_dir) => DocCheckResult {
            intact: false,
            current_hash: current,
            expected_hash: String::new(),
            message: format!(
                "{name} baseline hash is missing — integrity cannot be verified. \
                 Re-establish the baseline explicitly if this change is legitimate."
            ),
        },
        None => {
            if let Err(e) = store_doc_hash(doc, agent_dir, &current) {
                warn!(agent = agent_id, file = name, "Failed to store initial baseline: {e}");
            } else {
                mark_established(doc, agent_dir);
            }
            DocCheckResult {
                intact: true,
                expected_hash: current.clone(),
                current_hash: current,
                message: format!("{name} fingerprint initialized (first run)"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct TmpDir(PathBuf);
    impl Drop for TmpDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn agent(name: &str) -> (TmpDir, PathBuf) {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("ddc-doc-test-{}-{n}", std::process::id()));
        let dir = root.join("home").join("agents").join(name);
        fs::create_dir_all(&dir).unwrap();
        (TmpDir(root), dir)
    }

    #[test]
    fn instructions_baseline_lifecycle() {
        let (_t, dir) = agent("ins");
        let doc = ProtectedDoc::Instructions;
        assert_eq!(doc_drift(doc, &dir), DocDrift::None);
        fs::write(dir.join("INSTRUCTIONS.md"), "rules v1").unwrap();
        assert_eq!(doc_drift(doc, &dir), DocDrift::None, "no baseline yet");
        assert!(check_doc_integrity(doc, "ins", &dir).intact, "first run establishes");
        assert_eq!(doc_drift(doc, &dir), DocDrift::Intact);
        assert!(dir.parent().unwrap().parent().unwrap().join("soul_hashes/ins.instructions.hash").exists());
        fs::write(dir.join("INSTRUCTIONS.md"), "edited outside").unwrap();
        assert_eq!(doc_drift(doc, &dir), DocDrift::Drifted);
        assert!(!check_doc_integrity(doc, "ins", &dir).intact);
        accept_doc_change(doc, "ins", &dir).unwrap();
        assert_eq!(doc_drift(doc, &dir), DocDrift::Intact);
    }

    #[test]
    fn soul_and_instructions_baselines_are_separate() {
        let (_t, dir) = agent("both");
        fs::write(dir.join("SOUL.md"), "soul").unwrap();
        fs::write(dir.join("INSTRUCTIONS.md"), "ins").unwrap();
        accept_doc_change(ProtectedDoc::Soul, "both", &dir).unwrap();
        assert_eq!(doc_drift(ProtectedDoc::Soul, &dir), DocDrift::Intact);
        assert_eq!(doc_drift(ProtectedDoc::Instructions, &dir), DocDrift::None);
    }

    #[test]
    fn derived_write_keeps_drift_drifted() {
        let (_t, dir) = agent("rn");
        let doc = ProtectedDoc::Soul;
        fs::write(dir.join("SOUL.md"), "I am Old").unwrap();
        accept_doc_change(doc, "rn", &dir).unwrap();
        // Intact before the rename: the baseline follows.
        let pre = fingerprint_doc(doc, &dir).unwrap();
        fs::write(dir.join("SOUL.md"), "I am New").unwrap();
        assert!(accept_if_was_intact(doc, "rn", &dir, &pre).unwrap());
        assert_eq!(doc_drift(doc, &dir), DocDrift::Intact);
        // Drifted before the rename: still drifted after.
        fs::write(dir.join("SOUL.md"), "I am New, tampered").unwrap();
        let pre = fingerprint_doc(doc, &dir).unwrap();
        fs::write(dir.join("SOUL.md"), "I am Newer, tampered").unwrap();
        assert!(!accept_if_was_intact(doc, "rn", &dir, &pre).unwrap());
        assert_eq!(doc_drift(doc, &dir), DocDrift::Drifted);
    }

    #[test]
    fn missing_hash_after_baseline_is_not_accepted() {
        let (_t, dir) = agent("hs11");
        let doc = ProtectedDoc::Instructions;
        fs::write(dir.join("INSTRUCTIONS.md"), "v1").unwrap();
        accept_doc_change(doc, "hs11", &dir).unwrap();
        fs::remove_file(dir.parent().unwrap().parent().unwrap().join("soul_hashes/hs11.instructions.hash")).unwrap();
        assert_eq!(doc_drift(doc, &dir), DocDrift::Drifted);
        let pre = fingerprint_doc(doc, &dir).unwrap();
        assert!(!doc_baseline_matches(doc, &dir, &pre));
    }
}
