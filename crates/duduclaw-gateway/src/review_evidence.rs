//! Immutable review material. Integrity is independent of behavioral evidence.
//! Source paths come from server-owned artifact records, never RPC parameters.
use std::fs::File;
use std::io::{Read, Take};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::artifacts::TaskArtifact;
use crate::task_store::TaskAuthoritySnapshot;

pub mod audience;
pub mod download;

const MAX_REVIEW_FILE_BYTES: u64 = 64 * 1024 * 1024;
pub const REVIEW_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    SelfReport,
    Test,
    Operator,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntegrityStatus {
    Current,
    Stale,
    Unverified,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewArtifact {
    pub artifact_id: String,
    pub name: String,
    pub agent_id: String,
    pub archived_name: Option<String>,
    pub source_path: Option<String>,
    pub source_hash: Option<String>,
    pub archived_hash: Option<String>,
    pub integrity: IntegrityStatus,
    pub reasons: Vec<String>,
    pub evidence_kind: EvidenceKind,
    pub run_id: Option<String>,
    /// Empty inherits the enclosing task ACL; it never means public.
    pub audience: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewSnapshot {
    pub schema_version: u32,
    pub snapshot_id: String,
    pub snapshot_hash: String,
    pub task_id: String,
    pub authority_revision: i64,
    pub authority_snapshot_hash: String,
    /// Reference-only: this does not alter report/enforce ledger semantics.
    pub criteria_ledger: Option<Value>,
    pub artifacts: Vec<ReviewArtifact>,
    pub captured_at: String,
    pub audience: Vec<String>,
    pub gaps: Vec<String>,
    pub waiting_for: Option<String>,
    pub next_check_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewAcceptance {
    pub snapshot_id: String,
    pub snapshot_hash: String,
    pub accepted_by: String,
    pub accepted_at: String,
}

/// Inputs are authenticated transport identities, not content assertions.
/// Task ACL always applies, including for explicit audiences.
pub fn audience_allows(
    task_acl_allowed: bool,
    audience: &[String],
    trusted_keys: &[String],
) -> bool {
    // Role names written by AI role members never restrict a person.
    duduclaw_core::task_packet::audience_allows(
        task_acl_allowed,
        &audience::human_audience(audience),
        trusted_keys,
    )
}

fn hash_reader(reader: Take<File>) -> Result<String, String> {
    let mut reader = reader;
    let mut hash = Sha256::new();
    let mut buf = [0_u8; 32 * 1024];
    let mut read_bytes = 0_u64;
    loop {
        let n = reader.read(&mut buf).map_err(|_| "artifact_unreadable")?;
        if n == 0 {
            break;
        }
        read_bytes += n as u64;
        if read_bytes > MAX_REVIEW_FILE_BYTES {
            return Err("artifact_too_large".into());
        }
        hash.update(&buf[..n]);
    }
    Ok(hex::encode(hash.finalize()))
}

/// Per-call memo of file hashes, keyed by real path, size and mtime: one
/// request that projects many snapshots reads each file once (F5-D, P-M6).
#[derive(Default)]
pub struct HashCache {
    entries: std::collections::HashMap<(PathBuf, u64, Option<std::time::SystemTime>), String>,
}

fn guarded_hash(home: &Path, root: &Path, path: &Path) -> Result<String, String> {
    guarded_hash_cached(home, root, path, &mut HashCache::default())
}

fn guarded_hash_cached(
    home: &Path,
    root: &Path,
    path: &Path,
    cache: &mut HashCache,
) -> Result<String, String> {
    let relative_root = root
        .strip_prefix(home)
        .map_err(|_| "artifact_outside_home")?;
    let home = home
        .canonicalize()
        .map_err(|_| "artifact_root_unavailable")?;
    let expected_root = home.join(relative_root);
    let root = root
        .canonicalize()
        .map_err(|_| "artifact_root_unavailable")?;
    if root != expected_root {
        return Err("artifact_owner_symlink_refused".into());
    }
    if !root.starts_with(&home) {
        return Err("artifact_outside_home".into());
    }
    let path = path.canonicalize().map_err(|_| "artifact_missing")?;
    if !path.starts_with(&root) {
        return Err("artifact_outside_owner".into());
    }
    let file = File::open(&path).map_err(|_| "artifact_unreadable")?;
    let metadata = file.metadata().map_err(|_| "artifact_unreadable")?;
    if !metadata.is_file() || metadata.len() > MAX_REVIEW_FILE_BYTES {
        return Err("artifact_too_large".into());
    }
    let key = (path.clone(), metadata.len(), metadata.modified().ok());
    if let Some(hash) = cache.entries.get(&key) {
        return Ok(hash.clone());
    }
    let hash = hash_reader(file.take(MAX_REVIEW_FILE_BYTES + 1))?;
    cache.entries.insert(key, hash.clone());
    Ok(hash)
}

fn source_file(home: &Path, artifact: &ReviewArtifact) -> Option<(PathBuf, PathBuf)> {
    crate::files_api::attachments_dir(home, Some(artifact.agent_id.as_str()))?;
    let root = home.join("agents").join(&artifact.agent_id);
    artifact.source_path.as_ref().map(|p| {
        let path = Path::new(p);
        (
            root.clone(),
            if path.is_absolute() {
                path.to_path_buf()
            } else {
                root.join(path)
            },
        )
    })
}
fn archived_file(home: &Path, artifact: &ReviewArtifact) -> Option<(PathBuf, PathBuf)> {
    let root = crate::files_api::attachments_dir(
        home,
        (!artifact.agent_id.is_empty()).then_some(artifact.agent_id.as_str()),
    )?;
    let name = artifact.archived_name.as_ref()?;
    // Reuse the actual download path fence, including basename validation.
    let path = crate::files_api::resolve_attachment_file(&root, name).ok()?;
    Some((root, path))
}

/// A freshly captured file hash proves its bytes, not tests or acceptance.
/// Every captured row begins as self-report until server run evidence exists.
pub fn capture_artifact(
    home: &Path,
    task_id: &str,
    row: &TaskArtifact,
    audience: Vec<String>,
) -> ReviewArtifact {
    let mut artifact = ReviewArtifact {
        artifact_id: crate::approval::payload_hash(
            &serde_json::json!({
                "task": task_id,
                "agent": row.agent_id,
                "archive": row.archived_name,
                "source": row.source_path,
                "produced_at": row.produced_at
            }),
        ),
        name: row.name.clone(),
        agent_id: row.agent_id.clone(),
        archived_name: row.archived_name.clone(),
        source_path: row.source_path.clone(),
        source_hash: None,
        archived_hash: None,
        integrity: IntegrityStatus::Unverified,
        reasons: Vec::new(),
        evidence_kind: EvidenceKind::SelfReport,
        run_id: None,
        audience,
    };
    if let Some((root, path)) = source_file(home, &artifact) {
        match guarded_hash(home, &root, &path) {
            Ok(hash) => artifact.source_hash = Some(hash),
            Err(e) => artifact.reasons.push(e),
        }
    }
    if artifact.archived_name.is_some() {
        match archived_file(home, &artifact)
            .ok_or_else(|| "artifact_archive_unavailable".to_string())
            .and_then(|(r, p)| guarded_hash(home, &r, &p))
        {
            Ok(hash) => artifact.archived_hash = Some(hash),
            Err(e) => artifact.reasons.push(e.into()),
        }
    }
    if artifact.source_hash.is_some() || artifact.archived_hash.is_some() {
        artifact.integrity = if artifact.reasons.is_empty() {
            IntegrityStatus::Current
        } else {
            IntegrityStatus::Stale
        };
    } else if artifact.reasons.is_empty() {
        artifact.reasons.push("legacy_no_content_hash".into());
    }
    artifact
}

impl ReviewSnapshot {
    /// Hash excludes only its own digest, retaining exact captured material.
    pub fn compute_hash(&self) -> String {
        let mut value = serde_json::to_value(self).expect("serializable review snapshot");
        value
            .as_object_mut()
            .expect("review object")
            .remove("snapshot_hash");
        crate::approval::payload_hash(&value)
    }
    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != REVIEW_SCHEMA_VERSION
            || self.task_id.is_empty()
            || self.snapshot_id.is_empty()
            || self.authority_revision < 1
            || self.authority_snapshot_hash.is_empty()
            || self.snapshot_hash != self.compute_hash()
        {
            return Err("invalid review snapshot".into());
        }
        Ok(())
    }
    /// Returns a projection; the accepted immutable snapshot is never updated.
    pub fn current_artifacts(
        &self,
        home: &Path,
        current: &TaskAuthoritySnapshot,
    ) -> Vec<ReviewArtifact> {
        self.current_artifacts_cached(home, current, &mut HashCache::default())
    }

    /// [`Self::current_artifacts`] sharing a hash memo across snapshots.
    pub fn current_artifacts_cached(
        &self,
        home: &Path,
        current: &TaskAuthoritySnapshot,
        cache: &mut HashCache,
    ) -> Vec<ReviewArtifact> {
        let task_changed = current.task_id != self.task_id
            || current.revision != self.authority_revision
            || current.hash != self.authority_snapshot_hash;
        self.artifacts
            .iter()
            .cloned()
            .map(|mut a| {
                if task_changed {
                    a.reasons.push("task_authority_changed".into());
                    a.integrity = IntegrityStatus::Stale;
                }
                for (expected, resolved, missing) in [
                    (
                        a.source_hash.clone(),
                        source_file(home, &a),
                        "artifact_source_missing",
                    ),
                    (
                        a.archived_hash.clone(),
                        archived_file(home, &a),
                        "artifact_archive_missing",
                    ),
                ] {
                    if let Some(expected) = expected {
                        let result = resolved
                            .ok_or_else(|| missing.to_string())
                            .and_then(|(r, p)| guarded_hash_cached(home, &r, &p, cache));
                        match result {
                            Ok(actual) if actual == expected => {}
                            Ok(_) => {
                                a.integrity = IntegrityStatus::Stale;
                                a.reasons.push("artifact_content_changed".into());
                            }
                            Err(e) => {
                                a.integrity = IntegrityStatus::Stale;
                                a.reasons.push(e);
                            }
                        }
                    }
                }
                a.reasons.sort();
                a.reasons.dedup();
                a
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn audience_never_expands_task_acl_and_matches_exact_identity() {
        let private = vec!["user:alice".into()];
        assert!(!audience_allows(false, &[], &[]));
        assert!(audience_allows(true, &[], &[]));
        assert!(!audience_allows(
            true,
            &private,
            &["user:alice-evil".into()]
        ));
        assert!(audience_allows(true, &private, &["user:alice".into()]));
        assert!(!audience_allows(false, &private, &["user:alice".into()]));
    }
    #[test]
    fn captured_hash_does_not_upgrade_behavioral_evidence() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("agents/a");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("report.md"), b"first").unwrap();
        let row = TaskArtifact {
            name: "report.md".into(),
            archived_name: None,
            agent_id: "a".into(),
            origin: crate::artifacts::ArtifactOrigin::Produced,
            attribution: crate::artifacts::Attribution::Exact,
            produced_at: "2026-10-04T00:00:00Z".into(),
            size: Some(5),
            round: Some(1),
            channel: None,
            source_path: Some("report.md".into()),
            evidence: None,
        };
        let a = capture_artifact(dir.path(), "task", &row, vec![]);
        assert_eq!(a.integrity, IntegrityStatus::Current);
        assert_eq!(a.evidence_kind, EvidenceKind::SelfReport);
        let current = TaskAuthoritySnapshot {
            task_id: "task".into(),
            revision: 1,
            hash: "contract".into(),
            status: "done".into(),
            claimed_by: None,
            eligible: false,
        };
        let mut s = ReviewSnapshot {
            schema_version: 1,
            snapshot_id: "s".into(),
            snapshot_hash: String::new(),
            task_id: "task".into(),
            authority_revision: 1,
            authority_snapshot_hash: "contract".into(),
            criteria_ledger: None,
            artifacts: vec![a],
            captured_at: "2026-10-04T00:00:00Z".into(),
            audience: vec![],
            gaps: vec![],
            waiting_for: None,
            next_check_at: None,
        };
        s.snapshot_hash = s.compute_hash();
        assert!(s.validate().is_ok());
        let hash = s.snapshot_hash.clone();
        std::fs::write(root.join("report.md"), b"second").unwrap();
        assert_eq!(
            s.current_artifacts(dir.path(), &current)[0].integrity,
            IntegrityStatus::Stale
        );
        std::fs::remove_file(root.join("report.md")).unwrap();
        assert_eq!(
            s.current_artifacts(dir.path(), &current)[0].integrity,
            IntegrityStatus::Stale
        );
        assert_eq!(s.compute_hash(), hash);
        let mut changed = current;
        changed.revision = 2;
        assert!(
            s.current_artifacts(dir.path(), &changed)[0]
                .reasons
                .contains(&"task_authority_changed".into())
        );
    }
}
