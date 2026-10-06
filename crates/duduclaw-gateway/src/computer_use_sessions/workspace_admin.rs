//! Operator actions on workspaces (design §4.9, §5.5, §5.6, §8.6):
//! list / fence / revoke / regrant / renew / rebind runner / delete. Shared
//! by the admin RPCs and the operator CLI. Answers never carry file content.
//!
//! **Barrier** (§5.5): after the registry change, the session that held the
//! workspace gets its stop and lost flags, then this waits for the session
//! lock (not `try_lock`) — at most [`super::http::ACTION_BUDGET`] — and ends
//! it with [`EndReason::LeaseLost`]. When the call returns `Ok`, no further
//! click or keystroke of that AI session can happen.

use std::sync::atomic::Ordering;

use serde_json::{Value, json};

use super::workspace;
use super::{ComputerUseSessions, EndReason, Entry, ErrorCode, OpError};
use crate::computer_workspaces::{self as cw, WorkspaceState, WorkspaceStore, unix_now};

/// The event actor. The terminal is recorded as itself
/// ([`cw::cli_approval::UNVERIFIED_ACTOR`]), never as a verified operator.
fn actor(operator: &str) -> String {
    if operator == cw::cli_approval::UNVERIFIED_ACTOR {
        return operator.to_string();
    }
    format!("operator:{}", duduclaw_core::truncate_chars(operator, 64))
}

fn bad_id() -> OpError {
    OpError::new(ErrorCode::BadRequest, "workspace_id 格式不正確。")
}

/// The admin view of one row (no content, no paths).
pub fn admin_row(row: &cw::WorkspaceRow, now: i64) -> Value {
    json!({
        "workspace_id": row.workspace_id,
        "owner": row.owner_agent_id,
        "state": row.state.as_str(),
        "state_reason": row.state_reason,
        "runner_id": row.runner_id,
        "created_at": row.created_at,
        "last_attached_at": row.last_attached_at,
        "expires_at": row.expires_at,
        "image_digest": row.image_digest,
        "data_revision": row.data_revision,
        "bytes_used": row.bytes_used,
        "files_used": row.files_used,
        "permission_revision": row.permission_revision,
        "lease_epoch": row.lease_epoch,
        "leased": row.lease_active(now),
        "lease_until": row.lease_until,
    })
}

impl ComputerUseSessions {
    fn entry_holding(&self, workspace_id: &str) -> Option<Entry> {
        self.sessions
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .values()
            .find(|e| {
                e.shared
                    .workspace
                    .get()
                    .is_some_and(|a| a.lease.workspace_id == workspace_id)
            })
            .cloned()
    }

    /// The barrier: stop the holder session and wait until it has ended.
    /// Returns the barrier time (unix seconds).
    async fn barrier(&self, workspace_id: &str) -> Result<i64, OpError> {
        if let Some(entry) = self.entry_holding(workspace_id) {
            entry.shared.workspace.lost.store(true, Ordering::Release);
            entry.control.stopped.store(true, Ordering::Release);
            let locked =
                tokio::time::timeout(super::http::ACTION_BUDGET, entry.session.lock_owned()).await;
            let Ok(mut session) = locked else {
                return Err(OpError::new(
                    ErrorCode::Timeout,
                    "等待進行中的操作結束逾時；工作區已凍結，但 session 尚未確認停止。",
                ));
            };
            self.end_and_wait(&mut session, EndReason::LeaseLost).await;
        }
        Ok(unix_now())
    }

    async fn admin_store(
        &self,
        workspace_id: &str,
    ) -> Result<std::sync::Arc<WorkspaceStore>, OpError> {
        if !cw::paths::valid_workspace_id(workspace_id) {
            return Err(bad_id());
        }
        cw::shared::shared_async(&self.home)
            .await
            .map_err(workspace::store_error)
    }

    pub async fn admin_workspace_list(&self, owner: Option<&str>) -> Result<Value, OpError> {
        let store = cw::shared::shared_async(&self.home)
            .await
            .map_err(workspace::store_error)?;
        let now = unix_now();
        let rows: Vec<Value> = store
            .list_all()
            .map_err(workspace::store_error)?
            .iter()
            .filter(|r| owner.is_none_or(|o| r.owner_agent_id == o))
            .map(|r| admin_row(r, now))
            .collect();
        Ok(json!({"ok": true, "workspaces": rows}))
    }

    /// Freeze: the AI loses control at once (epoch +1), then the barrier.
    pub async fn admin_workspace_fence(
        &self,
        workspace_id: &str,
        operator: &str,
        reason: &str,
    ) -> Result<Value, OpError> {
        let store = self.admin_store(workspace_id).await?;
        store
            .fence(
                workspace_id,
                &actor(operator),
                &duduclaw_core::truncate_chars(reason, 200),
            )
            .map_err(workspace::store_error)?;
        let barrier_at = self.barrier(workspace_id).await?;
        Ok(json!({"ok": true, "workspace_id": workspace_id, "barrier_at": barrier_at}))
    }

    pub async fn admin_workspace_revoke(
        &self,
        workspace_id: &str,
        operator: &str,
    ) -> Result<Value, OpError> {
        let store = self.admin_store(workspace_id).await?;
        let row = store
            .get(workspace_id)
            .map_err(workspace::store_error)?
            .ok_or_else(workspace::ws_not_found)?;
        if !row.state.revocable() {
            return Err(workspace::state_error(row.state));
        }
        store
            .transition(
                workspace_id,
                &[row.state],
                WorkspaceState::Revoked,
                &actor(operator),
                "revoked",
                Some("operator_revoke"),
            )
            .map_err(workspace::store_error)?;
        let barrier_at = self.barrier(workspace_id).await?;
        Ok(
            json!({"ok": true, "workspace_id": workspace_id, "state": "revoked", "barrier_at": barrier_at}),
        )
    }

    pub async fn admin_workspace_regrant(
        &self,
        workspace_id: &str,
        operator: &str,
    ) -> Result<Value, OpError> {
        let store = self.admin_store(workspace_id).await?;
        let row = store
            .transition(
                workspace_id,
                &[WorkspaceState::Revoked],
                WorkspaceState::Ready,
                &actor(operator),
                "regranted",
                None,
            )
            .map_err(workspace::store_error)?;
        Ok(json!({"ok": true, "workspace": admin_row(&row, unix_now())}))
    }

    pub async fn admin_workspace_renew(
        &self,
        workspace_id: &str,
        operator: &str,
    ) -> Result<Value, OpError> {
        let store = self.admin_store(workspace_id).await?;
        let retention = cw::config::load(&self.home)
            .unwrap_or_default()
            .retention_days;
        let row = store
            .renew_retention(workspace_id, &actor(operator), unix_now(), retention)
            .map_err(workspace::store_error)?;
        Ok(json!({"ok": true, "workspace": admin_row(&row, unix_now())}))
    }

    /// Rebind to the runner this gateway sees now (never automatic).
    pub async fn admin_workspace_rebind_runner(
        &self,
        workspace_id: &str,
        operator: &str,
    ) -> Result<Value, OpError> {
        let store = self.admin_store(workspace_id).await?;
        let runner = self
            .workspace_rt
            .runner_id(&self.home)
            .await
            .ok_or_else(workspace::unavailable)?;
        store
            .rebind_runner(workspace_id, &actor(operator), &runner)
            .map_err(workspace::store_error)?;
        let barrier_at = self.barrier(workspace_id).await?;
        Ok(
            json!({"ok": true, "workspace_id": workspace_id, "runner_id": runner, "barrier_at": barrier_at}),
        )
    }

    /// Delete (operator only, D5): `deleting` + fence, barrier, remove the
    /// derived directory, `deleted` tombstone. A failure stays `deleting`
    /// and boot reconciliation retries.
    pub async fn admin_workspace_delete(
        &self,
        workspace_id: &str,
        operator: &str,
    ) -> Result<Value, OpError> {
        let store = self.admin_store(workspace_id).await?;
        let row = store
            .get(workspace_id)
            .map_err(workspace::store_error)?
            .ok_or_else(workspace::ws_not_found)?;
        if row.state != WorkspaceState::Deleting {
            if !row.state.deletable() {
                return Err(workspace::state_error(row.state));
            }
            store
                .transition(
                    workspace_id,
                    &[row.state],
                    WorkspaceState::Deleting,
                    &actor(operator),
                    "delete_started",
                    Some("operator_delete"),
                )
                .map_err(workspace::store_error)?;
        }
        self.barrier(workspace_id).await?;
        let (home, id) = (self.home.clone(), workspace_id.to_string());
        // Under the workspace lock: no write or reconciliation is midway.
        let removed = tokio::task::spawn_blocking(move || {
            let _lock = cw::lock::lock_for_change(&home, &id)
                .map_err(|_| std::io::Error::other("workspace busy"))?;
            cw::paths::remove_workspace_dir(&home, &id)
        })
        .await
        .map_err(|_| workspace::unavailable())?;
        if removed.is_err() {
            return Err(OpError::new(
                ErrorCode::WorkspaceUnavailable,
                "工作區目錄刪除失敗，狀態維持 deleting，gateway 下次開機會重試；請執行 duduclaw doctor 檢查。",
            ));
        }
        store
            .transition(
                workspace_id,
                &[WorkspaceState::Deleting],
                WorkspaceState::Deleted,
                &actor(operator),
                "deleted",
                None,
            )
            .map_err(workspace::store_error)?;
        cw::owner_cred::forget(&self.home, &row.owner_agent_id, workspace_id);
        Ok(json!({"ok": true, "workspace_id": workspace_id, "state": "deleted"}))
    }
}

/// The registry operator actions run against: the live gateway's (so the
/// barrier reaches its sessions), else a session-less one over `home` (a
/// process with no live sessions has nothing to wait for).
pub fn admin_sessions(home: &std::path::Path) -> std::sync::Arc<ComputerUseSessions> {
    let live = super::active_registry()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .upgrade();
    match live {
        Some(s) if s.home() == home => s,
        _ => std::sync::Arc::new(ComputerUseSessions::with_parts(
            home.to_path_buf(),
            super::backend::orchestrator_factory(),
            super::IDLE_TIMEOUT,
        )),
    }
}
