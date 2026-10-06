//! `tasks.artifacts` evidence projection (F3, V-M-9 / L-4 / L-5).
//!
//! The provenance ledger (`artifacts.jsonl`) is a plain file: a row's
//! `evidence` reference there is a pointer, never the evidence itself. A row
//! is shown with an evidence kind only after the pointed-to review snapshot
//! is loaded from the workflow store, its hash re-verified and the artifact
//! found in it; the kind, hash and run come from that stored record. The
//! task's latest snapshot that holds the artifact wins over an older pointer.
use super::*;
use crate::artifacts::TaskArtifact;
use crate::review_evidence::{ReviewArtifact, ReviewSnapshot};

/// One artifact row on the wire, with verified evidence or none.
pub(crate) fn artifact_row_json(
    home: &Path,
    row: &TaskArtifact,
    task_id: &str,
    current: &crate::task_store::TaskAuthoritySnapshot,
    reader: &UserContext,
    candidates: &[ReviewSnapshot],
    projections: &mut Projections,
) -> Value {
    let mut value = row.to_wire_json();
    // L-5: the reader needs to know a source file exists, not where it is.
    value["source_path"] = json!(row.source_path.as_deref().map(|p| {
        Path::new(p)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string()
    }));
    value["evidence"] = Value::Null;
    value["integrity"] = json!("unverified");
    value["stale_reasons"] = json!(["legacy_no_review_snapshot"]);
    let reference = row.evidence.as_ref();
    let pick = candidates.iter().find_map(|s| {
        let pointed = reference
            .is_some_and(|r| r.snapshot_id == s.snapshot_id && r.snapshot_hash == s.snapshot_hash);
        let latest = candidates
            .first()
            .is_some_and(|l| l.snapshot_id == s.snapshot_id);
        if !(pointed || latest)
            || s.task_id != task_id
            || s.validate().is_err()
            || !crate::review_evidence::audience::dashboard_may_read(reader, &s.audience)
        {
            return None;
        }
        s.artifacts
            .iter()
            .position(|a| same_artifact(a, row))
            .map(|i| (s, i))
    });
    if let Some((snapshot, index)) = pick {
        let stored = &snapshot.artifacts[index];
        value["evidence"] = json!({
            "snapshot_id": snapshot.snapshot_id,
            "snapshot_hash": snapshot.snapshot_hash,
            "task_revision": snapshot.authority_revision,
            "content_hash": stored.archived_hash.clone().or_else(|| stored.source_hash.clone()),
            "evidence_kind": stored.evidence_kind,
            "run_id": stored.run_id,
            "snapshot_verified": true,
        });
        if let Some(a) = projections.of(home, current, snapshot).get(index) {
            value["integrity"] = json!(a.integrity);
            value["stale_reasons"] = json!(a.reasons);
        }
    }
    value
}

/// Current-state projections for one `tasks.artifacts` call: each snapshot
/// is projected once and each file hashed once (F5-D, P-M6), instead of once
/// per artifact row.
#[derive(Default)]
pub(crate) struct Projections {
    by_snapshot: HashMap<String, Vec<ReviewArtifact>>,
    hashes: crate::review_evidence::HashCache,
}

impl Projections {
    fn of(
        &mut self,
        home: &Path,
        current: &crate::task_store::TaskAuthoritySnapshot,
        snapshot: &ReviewSnapshot,
    ) -> &[ReviewArtifact] {
        if !self.by_snapshot.contains_key(&snapshot.snapshot_id) {
            let projected = snapshot.current_artifacts_cached(home, current, &mut self.hashes);
            self.by_snapshot
                .insert(snapshot.snapshot_id.clone(), projected);
        }
        &self.by_snapshot[&snapshot.snapshot_id]
    }
}

fn same_artifact(a: &ReviewArtifact, row: &TaskArtifact) -> bool {
    a.agent_id == row.agent_id
        && a.archived_name == row.archived_name
        && a.source_path == row.source_path
}

/// Snapshots to consider for a task's artifact rows: the latest first, then
/// every snapshot a ledger row points at (each loaded once).
pub(crate) async fn evidence_candidates(
    workflows: &crate::workflow::WorkflowStore,
    task_id: &str,
    rows: &[TaskArtifact],
) -> Vec<ReviewSnapshot> {
    let mut out: Vec<ReviewSnapshot> = Vec::new();
    if let Ok(Some(latest)) = workflows.latest_review_snapshot(task_id).await {
        out.push(latest);
    }
    for r in rows.iter().filter_map(|r| r.evidence.as_ref()) {
        if out.iter().any(|s| s.snapshot_id == r.snapshot_id) {
            continue;
        }
        if let Ok(Some(s)) = workflows.review_snapshot(&r.snapshot_id).await {
            out.push(s);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifacts::{ArtifactEvidenceRef, ArtifactOrigin, Attribution};
    use crate::review_evidence::{EvidenceKind, IntegrityStatus};

    fn row(evidence: Option<ArtifactEvidenceRef>) -> TaskArtifact {
        TaskArtifact {
            name: "report.md".into(),
            archived_name: None,
            agent_id: "sales".into(),
            origin: ArtifactOrigin::Produced,
            attribution: Attribution::Exact,
            produced_at: "2026-10-04T00:00:00Z".into(),
            size: Some(1),
            round: Some(1),
            channel: None,
            source_path: Some("/home/x/agents/sales/report.md".into()),
            evidence,
        }
    }
    fn snapshot(id: &str, kind: EvidenceKind) -> ReviewSnapshot {
        let mut s = ReviewSnapshot {
            schema_version: 1,
            snapshot_id: id.into(),
            snapshot_hash: String::new(),
            task_id: "task".into(),
            authority_revision: 1,
            authority_snapshot_hash: "h".into(),
            criteria_ledger: None,
            artifacts: vec![ReviewArtifact {
                artifact_id: "a".into(),
                name: "report.md".into(),
                agent_id: "sales".into(),
                archived_name: None,
                source_path: Some("/home/x/agents/sales/report.md".into()),
                source_hash: Some("content".into()),
                archived_hash: None,
                integrity: IntegrityStatus::Current,
                reasons: vec![],
                evidence_kind: kind,
                run_id: None,
                audience: vec![],
            }],
            captured_at: "2026-10-04T00:00:00Z".into(),
            audience: vec![],
            gaps: vec![],
            waiting_for: None,
            next_check_at: None,
        };
        s.snapshot_hash = s.compute_hash();
        s
    }
    fn reference(s: &ReviewSnapshot, kind: EvidenceKind) -> ArtifactEvidenceRef {
        ArtifactEvidenceRef {
            task_revision: 1,
            snapshot_id: s.snapshot_id.clone(),
            snapshot_hash: s.snapshot_hash.clone(),
            content_hash: None,
            evidence_kind: kind,
            run_id: None,
            audience: vec![],
        }
    }
    fn current() -> crate::task_store::TaskAuthoritySnapshot {
        crate::task_store::TaskAuthoritySnapshot {
            task_id: "task".into(),
            revision: 1,
            hash: "h".into(),
            status: "done".into(),
            claimed_by: None,
            eligible: false,
        }
    }

    #[test]
    fn ledger_kind_is_never_trusted_and_unverified_rows_carry_no_evidence() {
        let home = tempfile::tempdir().unwrap();
        let s = snapshot("s1", EvidenceKind::SelfReport);
        // A forged ledger row claims operator acceptance.
        let forged = row(Some(reference(&s, EvidenceKind::Operator)));
        let v = artifact_row_json(
            home.path(),
            &forged,
            "task",
            &current(),
            &UserContext::admin_fallback(),
            &[s.clone()],
            &mut Projections::default(),
        );
        assert_eq!(v["evidence"]["evidence_kind"], "self_report");
        assert_eq!(v["evidence"]["snapshot_verified"], true);
        assert_eq!(
            v["source_path"], "report.md",
            "no absolute path on the wire"
        );
        // Pointer to a snapshot whose hash does not match, and no latest.
        let mut wrong = reference(&s, EvidenceKind::Operator);
        wrong.snapshot_hash = "tampered".into();
        let mut older = s.clone();
        older.snapshot_id = "other".into();
        older.snapshot_hash = older.compute_hash();
        let v = artifact_row_json(
            home.path(),
            &row(Some(wrong)),
            "task",
            &current(),
            &UserContext::admin_fallback(),
            &[snapshot("latest-without-it", EvidenceKind::Test)]
                .into_iter()
                .map(|mut l| {
                    l.artifacts.clear();
                    l.snapshot_hash = l.compute_hash();
                    l
                })
                .chain([older])
                .collect::<Vec<_>>(),
            &mut Projections::default(),
        );
        assert!(v["evidence"].is_null());
        // A snapshot record edited after capture fails its hash check.
        let mut edited = s.clone();
        edited.artifacts[0].evidence_kind = EvidenceKind::Operator;
        let v = artifact_row_json(
            home.path(),
            &row(Some(reference(&s, EvidenceKind::Operator))),
            "task",
            &current(),
            &UserContext::admin_fallback(),
            &[edited],
            &mut Projections::default(),
        );
        assert!(v["evidence"].is_null());
    }

    #[test]
    fn latest_snapshot_wins_over_an_older_pointer() {
        let home = tempfile::tempdir().unwrap();
        let old = snapshot("old", EvidenceKind::SelfReport);
        let latest = snapshot("new", EvidenceKind::Test);
        let r = row(Some(reference(&old, EvidenceKind::SelfReport)));
        let v = artifact_row_json(
            home.path(),
            &r,
            "task",
            &current(),
            &UserContext::admin_fallback(),
            &[latest, old],
            &mut Projections::default(),
        );
        assert_eq!(v["evidence"]["snapshot_id"], "new");
        assert_eq!(v["evidence"]["evidence_kind"], "test");
    }
}
