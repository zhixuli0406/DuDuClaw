//! Fixed review snapshots; authenticated task ACL plus TaskPacket audience.
use super::*;
use crate::review_evidence::audience::{
    TaskAudience, dashboard_may_read, dashboard_may_read_task, task_audience,
};
use crate::review_evidence::{
    IntegrityStatus, REVIEW_SCHEMA_VERSION, ReviewAcceptance, ReviewSnapshot,
};

/// Gap recorded when a task had more deliverables than one snapshot holds.
pub(crate) const TRUNCATED_GAP: &str = "artifact_snapshot_truncated";

impl MethodHandler {
    pub(crate) async fn handle_tasks_review_snapshot(
        &self,
        params: Value,
        ctx: &UserContext,
    ) -> WsFrame {
        let fresh =
            match crate::review_evidence::audience::fresh_dashboard_context(&self.home_dir, ctx) {
                Ok(c) => c,
                Err(_) => return WsFrame::error_response("", "permission denied"),
            };
        let ctx = &fresh;
        let task_id = params.get("task_id").and_then(Value::as_str).unwrap_or("");
        if task_id.is_empty() {
            return WsFrame::error_response("", "task_id is required");
        }
        let tasks = match self.task_store().await {
            Ok(s) => s,
            Err(f) => return f,
        };
        let task = match self
            .authorize_task_access(&tasks, ctx, task_id, AccessLevel::Viewer)
            .await
        {
            Ok(t) => t,
            Err(f) => return f,
        };
        let task_aud = task_audience(&self.home_dir, task_id);
        if !dashboard_may_read_task(ctx, &task_aud) {
            return WsFrame::error_response("", "permission denied");
        }
        let audience = task_aud.keys().to_vec();
        let store = match self.workflow_store().await {
            Ok(s) => s,
            Err(e) => return WsFrame::error_response("", &e),
        };
        let action = params
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("get");
        let snapshot = if action == "capture" {
            // An unreadable packet set has no known audience; a snapshot
            // recorded now would look unrestricted.
            if task_aud == TaskAudience::Unreadable {
                return WsFrame::error_response("", "task audience unavailable");
            }
            if !ctx.has_role(UserRole::Manager) {
                return WsFrame::error_response("", "manager role required");
            }
            // V-M-3: capturing writes an immutable record; Operator binding
            // on the task's employee, like creating a draft.
            if !ctx.has_agent_access(&task.assigned_to, AccessLevel::Operator) {
                return WsFrame::error_response("", "permission denied");
            }
            let owner = task
                .claimed_by
                .as_deref()
                .filter(|s| !s.is_empty())
                .unwrap_or(&task.assigned_to);
            let rows = crate::artifacts::collect_task_artifacts(
                &self.home_dir,
                task_id,
                owner,
                task.claimed_at.as_deref().unwrap_or(&task.created_at),
                task.completed_at.as_deref().unwrap_or(&task.updated_at),
                32,
            );
            let artifacts = rows
                .artifacts
                .iter()
                .map(|a| {
                    crate::review_evidence::capture_artifact(
                        &self.home_dir,
                        task_id,
                        a,
                        audience.clone(),
                    )
                })
                .collect();
            let mut snapshot = ReviewSnapshot {
                schema_version: REVIEW_SCHEMA_VERSION,
                snapshot_id: uuid::Uuid::new_v4().to_string(),
                snapshot_hash: String::new(),
                task_id: task.id.clone(),
                authority_revision: task.authority_revision,
                authority_snapshot_hash: task.authority_snapshot_hash(),
                criteria_ledger: task
                    .criteria_ledger
                    .as_deref()
                    .and_then(|s| serde_json::from_str(s).ok()),
                artifacts,
                captured_at: Utc::now().to_rfc3339(),
                audience: audience.clone(),
                gaps: task.blocked_reason.iter().cloned().collect(),
                waiting_for: task.pause_reason.clone(),
                next_check_at: None,
            };
            if rows.truncated {
                snapshot.gaps.push(TRUNCATED_GAP.into());
            }
            if snapshot.artifacts.is_empty() {
                snapshot.gaps.push("no_recorded_deliverable".into());
            }
            snapshot.snapshot_hash = snapshot.compute_hash();
            if let Err(e) = store.save_review_snapshot(&snapshot).await {
                return WsFrame::error_response("", &e);
            }
            for (row, artifact) in rows.artifacts.iter().zip(&snapshot.artifacts) {
                crate::artifacts::record_review_reference(
                    &self.home_dir,
                    row,
                    task_id,
                    crate::artifacts::ArtifactEvidenceRef {
                        task_revision: snapshot.authority_revision,
                        snapshot_id: snapshot.snapshot_id.clone(),
                        snapshot_hash: snapshot.snapshot_hash.clone(),
                        content_hash: artifact
                            .archived_hash
                            .clone()
                            .or_else(|| artifact.source_hash.clone()),
                        evidence_kind: artifact.evidence_kind,
                        run_id: artifact.run_id.clone(),
                        audience: artifact.audience.clone(),
                    },
                );
            }
            Some(snapshot)
        } else if action == "get" {
            let loaded = if let Some(id) = params.get("snapshot_id").and_then(Value::as_str) {
                store.review_snapshot(id).await
            } else {
                store.latest_review_snapshot(task_id).await
            };
            match loaded {
                Ok(s) => s,
                Err(e) => return WsFrame::error_response("", &e),
            }
        } else {
            return WsFrame::error_response("", "unknown review action");
        };
        let Some(snapshot) = snapshot else {
            return WsFrame::ok_response("", json!({"snapshot":null}));
        };
        if snapshot.task_id != task_id || !dashboard_may_read(ctx, &snapshot.audience) {
            return WsFrame::error_response("", "permission denied");
        }
        let current = crate::task_store::TaskAuthoritySnapshot {
            task_id: task.id.clone(),
            revision: task.authority_revision,
            hash: task.authority_snapshot_hash(),
            status: task.status.clone(),
            claimed_by: task.claimed_by.clone(),
            eligible: task.approval_eligible(),
        };
        let current_artifacts = snapshot.current_artifacts(&self.home_dir, &current);
        let authority_current = current.revision == snapshot.authority_revision
            && current.hash == snapshot.authority_snapshot_hash;
        let acceptance = match store.review_acceptance(&snapshot.snapshot_id).await {
            Ok(a) => a,
            Err(e) => return WsFrame::error_response("", &e),
        };
        WsFrame::ok_response(
            "",
            json!({
                "snapshot": snapshot,
                "current_artifacts": current_artifacts,
                "authority_current": authority_current,
                "acceptance": acceptance
            }),
        )
    }
    pub(crate) async fn handle_tasks_review_accept(
        &self,
        params: Value,
        ctx: &UserContext,
    ) -> WsFrame {
        let fresh =
            match crate::review_evidence::audience::fresh_dashboard_context(&self.home_dir, ctx) {
                Ok(c) => c,
                Err(_) => return WsFrame::error_response("", "permission denied"),
            };
        let ctx = &fresh;
        if !ctx.has_role(UserRole::Manager) {
            return WsFrame::error_response("", "manager role required");
        }
        let task_id = params.get("task_id").and_then(Value::as_str).unwrap_or("");
        let snapshot_id = params
            .get("snapshot_id")
            .and_then(Value::as_str)
            .unwrap_or("");
        let hash = params
            .get("snapshot_hash")
            .and_then(Value::as_str)
            .unwrap_or("");
        let tasks = match self.task_store().await {
            Ok(s) => s,
            Err(f) => return f,
        };
        // V-M-3: accepting is a decision, Operator on the task's employee.
        let task = match self
            .authorize_task_access(&tasks, ctx, task_id, AccessLevel::Operator)
            .await
        {
            Ok(t) => t,
            Err(f) => return f,
        };
        let store = match self.workflow_store().await {
            Ok(s) => s,
            Err(e) => return WsFrame::error_response("", &e),
        };
        let snapshot = match store.review_snapshot(snapshot_id).await {
            Ok(Some(s)) => s,
            Ok(None) => return WsFrame::error_response("", "review snapshot not found"),
            Err(e) => return WsFrame::error_response("", &e),
        };
        if snapshot.task_id != task_id
            || !dashboard_may_read(ctx, &snapshot.audience)
            || !dashboard_may_read_task(ctx, &task_audience(&self.home_dir, task_id))
        {
            return WsFrame::error_response("", "permission denied");
        }
        if hash != snapshot.snapshot_hash {
            return WsFrame::error_response("", "fixed snapshot hash mismatch");
        }
        // V-M-8: only the task's latest snapshot is acceptable. A page that
        // kept an older one on screen while another reviewer captured a new
        // one sends the old id and hash and is refused.
        match store.latest_review_snapshot(task_id).await {
            Ok(Some(latest)) if latest.snapshot_id == snapshot.snapshot_id => {}
            Ok(_) => return WsFrame::error_response("", "review snapshot is not the latest"),
            Err(e) => return WsFrame::error_response("", &e),
        }
        // L-3: a snapshot that could not hold every deliverable is not a
        // complete record of what is being accepted.
        if snapshot.gaps.iter().any(|g| g == TRUNCATED_GAP) {
            return WsFrame::error_response("", "review snapshot truncated");
        }
        let current = crate::task_store::TaskAuthoritySnapshot {
            task_id: task.id.clone(),
            revision: task.authority_revision,
            hash: task.authority_snapshot_hash(),
            status: task.status.clone(),
            claimed_by: task.claimed_by.clone(),
            eligible: task.approval_eligible(),
        };
        if current.revision != snapshot.authority_revision
            || current.hash != snapshot.authority_snapshot_hash
            || snapshot.artifacts.is_empty()
            || snapshot
                .current_artifacts(&self.home_dir, &current)
                .iter()
                .any(|a| a.integrity != IntegrityStatus::Current)
        {
            return WsFrame::error_response("", "review snapshot stale or unverified");
        }
        let acceptance = ReviewAcceptance {
            snapshot_id: snapshot.snapshot_id,
            snapshot_hash: snapshot.snapshot_hash,
            accepted_by: ctx.user_id.clone(),
            accepted_at: Utc::now().to_rfc3339(),
        };
        match store.accept_review_snapshot(&acceptance).await {
            Ok(()) => WsFrame::ok_response("", json!({"acceptance":acceptance})),
            Err(e) => WsFrame::error_response("", &e),
        }
    }
}
