//! Durable workspaces in tool-driven sessions (P2-C design §4–§7).
//!
//! A session started with `workspace` holds a registry lease
//! ([`crate::computer_workspaces`]). The lease lives in [`SessionShared`] so
//! the reaper renews it without the session lock; `check_alive`,
//! `final_control_gate` and the confirmation poll re-read the registry, and
//! a lost lease ends the session with [`EndReason::LeaseLost`]. Without
//! `workspace` none of this runs and the session is byte-identical to before.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use serde_json::{Value, json};
use tracing::{debug, warn};

use super::{ComputerUseSessions, EndReason, ErrorCode, ManagedSession, OpError, SessionShared};
use crate::computer_use_image::Presence;
use crate::computer_use_orchestrator::{ComputerUseConfig, WorkspaceMount};
use crate::computer_workspaces::files::{FileError, MAX_FILE_BYTES};
use crate::computer_workspaces::store::AcquireRequest;
use crate::computer_workspaces::{
    self as cw, LEASE_TTL_SECS, Lease, StoreError, WorkspaceState, WorkspacesConfig, paths,
    unix_now,
};

/// What a session holds once a workspace is attached.
#[derive(Debug, Clone)]
pub(crate) struct AttachedWorkspace {
    pub lease: Lease,
    pub image_digest: String,
}

/// Per-session workspace state reachable without the session lock.
#[derive(Default)]
pub(crate) struct WorkspaceSlot {
    pub(crate) attached: std::sync::Mutex<Option<AttachedWorkspace>>,
    /// Set by the reaper (renewal failed) or an operator fence/revoke.
    pub(crate) lost: AtomicBool,
}

impl WorkspaceSlot {
    pub(crate) fn get(&self) -> Option<AttachedWorkspace> {
        self.attached
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }
    fn take(&self) -> Option<AttachedWorkspace> {
        self.attached
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take()
    }
}

/// Docker facts a workspace start needs; tests inject a fake.
#[async_trait]
pub(crate) trait WorkspaceRuntime: Send + Sync {
    async fn runner_id(&self, home: &Path) -> Option<String>;
    async fn image_id(&self, image: &str) -> Result<String, Presence>;
}

pub(crate) struct DockerWorkspaceRuntime;

#[async_trait]
impl WorkspaceRuntime for DockerWorkspaceRuntime {
    async fn runner_id(&self, home: &Path) -> Option<String> {
        cw::current_runner_id(home).await
    }
    async fn image_id(&self, image: &str) -> Result<String, Presence> {
        crate::computer_use_image::image_id(image).await
    }
}

/// The lease a start takes: the normal TTL plus the whole start budget, so
/// a start that waits for an approval and a slow container never loses its
/// lease before the reaper can renew it (the reaper only renews sessions
/// that finished starting).
pub(crate) const INITIAL_LEASE_TTL_SECS: i64 =
    LEASE_TTL_SECS + super::http::START_BUDGET.as_secs() as i64;

/// This gateway process's instance id (`lease_instance`).
fn instance_id() -> &'static str {
    static ID: OnceLock<String> = OnceLock::new();
    ID.get_or_init(|| uuid::Uuid::new_v4().as_simple().to_string())
}

// ── refusals ─────────────────────────────────────────────────────────────

/// "No such workspace" — the same bytes whether it does not exist, belongs
/// to someone else, or the id is malformed.
pub(crate) fn ws_not_found() -> OpError {
    OpError::new(ErrorCode::NotFound, "找不到這個電腦操作工作區。")
}

pub(crate) fn disabled() -> OpError {
    OpError::new(
        ErrorCode::WorkspaceDisabled,
        "電腦操作工作區未啟用（需要 config.toml [computer_use.workspaces] enabled 與此員工的 [capabilities.computer_use_config] workspace 同時為 true）。",
    )
}

pub(crate) fn unavailable() -> OpError {
    OpError::new(
        ErrorCode::WorkspaceUnavailable,
        "電腦操作工作區目前無法使用，請管理員執行 duduclaw doctor 檢查。",
    )
}

pub(crate) fn lease_lost() -> OpError {
    OpError::new(
        ErrorCode::LeaseLost,
        "這個工作區目前沒有由你進行中的電腦操作 session 掛載（或掛載已失效）。請先以 computer_session_start 帶 workspace 參數重新掛載。",
    )
}

pub(crate) fn state_error(state: WorkspaceState) -> OpError {
    OpError::new(ErrorCode::WorkspaceState, state.blocked_message())
}

pub(crate) fn store_error(e: StoreError) -> OpError {
    match e {
        StoreError::NotFound => ws_not_found(),
        StoreError::State(s) => state_error(s),
        StoreError::RunnerMismatch => OpError::new(
            ErrorCode::RunnerMismatch,
            "工作區綁定在另一個 Docker 環境，請管理員確認後改綁。",
        ),
        StoreError::Busy => OpError::new(
            ErrorCode::WorkspaceBusy,
            "另一個 session 正在使用這個工作區；對方若已離線，一般最多 90 秒後會自動釋放，若對方是在啟動途中中斷，最多約 8.5 分鐘。",
        ),
        StoreError::Quota => OpError::new(
            ErrorCode::WorkspaceQuota,
            "此員工的工作區數量已達上限（[computer_use.workspaces] max_per_agent）。",
        ),
        StoreError::LeaseLost => lease_lost(),
        StoreError::RevisionMismatch(r) => OpError::new(
            ErrorCode::WorkspaceState,
            format!("工作區內容已被更新（目前版本 {r}），請重新讀取後再寫入。"),
        ),
        StoreError::Unavailable(why) => {
            warn!(%why, "computer workspace registry unavailable");
            unavailable()
        }
    }
}

pub(crate) fn file_error(e: FileError, cfg: &WorkspacesConfig) -> OpError {
    match e {
        FileError::InvalidPath => OpError::new(
            ErrorCode::BadRequest,
            "路徑不符合規則：相對路徑、最多 4 層、每段只能用文字、數字、空白與 -_.()（），不能以 . 開頭。",
        ),
        FileError::TooLarge => OpError::new(
            ErrorCode::PayloadTooLarge,
            format!(
                "單次讀寫上限是 {} KiB 的 UTF-8 文字。",
                MAX_FILE_BYTES / 1024
            ),
        ),
        FileError::NotUtf8 => {
            OpError::new(ErrorCode::BadRequest, "這個檔案不是 UTF-8 文字，無法讀取。")
        }
        FileError::FileNotFound => OpError::new(ErrorCode::NotFound, "工作區裡沒有這個檔案。"),
        FileError::Quota {
            bytes_used,
            files_used,
        } => OpError::new(
            ErrorCode::WorkspaceQuota,
            format!(
                "工作區已達配額（目前 {bytes_used} / {} 位元組、{files_used} / {} 個檔案），這次寫入沒有執行，原本的檔案都還在。",
                cfg.max_bytes, cfg.max_files
            ),
        ),
        FileError::DiskFull => OpError::new(
            ErrorCode::DiskFull,
            "主機磁碟剩餘空間不足，這次寫入沒有執行，工作區裡原本的檔案都還在。",
        ),
        FileError::LeaseLost => lease_lost(),
        FileError::LandedAfterFence => OpError::new(
            ErrorCode::LeaseLost,
            "工作區的掛載在寫入途中失效：檔案已寫入，但這個 session 已不再擁有工作區。請重新以 computer_session_start 掛載後確認內容。",
        ),
        FileError::Unprocessable => OpError::new(
            ErrorCode::BadRequest,
            "這個路徑上的項目系統無法處理（多重連結、連結、特殊檔案或資料夾），這次沒有讀寫任何內容。",
        ),
        FileError::SessionHalted => OpError::new(
            ErrorCode::Paused,
            "電腦操作 session 已停止或暫停（或威脅等級不是 GREEN），這次寫入沒有執行，原本的檔案都還在。",
        ),
        FileError::Busy => OpError::new(
            ErrorCode::WorkspaceBusy,
            "這個工作區正在處理另一筆寫入，請稍後再試；這次寫入沒有執行。",
        ),
        FileError::RevisionMismatch(r) => store_error(StoreError::RevisionMismatch(r)),
        FileError::Unavailable => unavailable(),
    }
}

/// The feature is usable for `agent_id` right now (both switches, unix).
pub(crate) fn feature_config(home: &Path, agent_id: &str) -> Result<WorkspacesConfig, OpError> {
    let cfg = cw::config::active(home).ok_or_else(disabled)?;
    if !cw::config::agent_enabled(home, agent_id) {
        return Err(disabled());
    }
    Ok(cfg)
}

// ── lease checks (sync: also used by `final_control_gate`) ───────────────

/// Why an attached session must end, if it must. `None` when nothing is
/// attached or the lease is still current and both switches are on.
pub(crate) fn lease_problem(
    home: &Path,
    agent_id: &str,
    shared: &SessionShared,
) -> Option<EndReason> {
    let attached = shared.workspace.get()?;
    if shared.workspace.lost.load(Ordering::Acquire) {
        return Some(EndReason::LeaseLost);
    }
    if feature_config(home, agent_id).is_err() {
        return Some(EndReason::LeaseLost);
    }
    // The handle was opened (off the async threads) when the workspace was
    // attached; a missing one fails closed instead of opening here.
    let Some(store) = cw::shared::cached(home) else {
        return Some(EndReason::LeaseLost);
    };
    match store.lease_current(&attached.lease, unix_now()) {
        Ok(true) => None,
        _ => Some(EndReason::LeaseLost),
    }
}

/// The `environment_hash` input of a confirmation binding: unchanged
/// `{session, display}` without a workspace (B.4 1), extended with the
/// workspace, epoch, permission revision and image id with one.
pub(crate) fn environment_input(session: &ManagedSession) -> Value {
    let display = [session.config.display_width, session.config.display_height];
    match session.shared.workspace.get() {
        None => json!({"session": session.session_id, "display": display}),
        Some(a) => json!({
            "session": session.session_id,
            "display": display,
            "workspace_id": a.lease.workspace_id,
            "lease_epoch": a.lease.epoch,
            "permission_revision": a.lease.permission_revision,
            "image_digest": a.image_digest,
        }),
    }
}

/// End-of-session lease handling, run on the detached end task after the
/// container is gone: CAS release (or a CAS self-fence for a lost lease).
pub(crate) fn take_lease(shared: &SessionShared) -> Option<Lease> {
    shared.workspace.take().map(|a| a.lease)
}

pub(crate) async fn release_lease(home: std::path::PathBuf, lease: Lease, reason: EndReason) {
    let _ = tokio::task::spawn_blocking(move || {
        let Ok(store) = cw::shared::shared_blocking(&home) else {
            warn!("computer workspace lease not released: registry unavailable (expires on its own)");
            return;
        };
        let done = if reason == EndReason::LeaseLost {
            store.fence_own(&lease, "lease_lost")
        } else {
            store.release(&lease)
        };
        if !matches!(done, Ok(true)) {
            debug!(workspace = %lease.workspace_id, "computer workspace lease already moved on; nothing released");
        }
    })
    .await;
}

impl ComputerUseSessions {
    /// Renew every attached lease (reaper; no session lock). A failed
    /// renewal marks the session lost and raises its stop flag.
    pub(super) async fn renew_leases(&self) {
        let entries: Vec<_> = self
            .sessions
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .values()
            .cloned()
            .collect();
        for entry in entries {
            let Some(attached) = entry.shared.workspace.get() else {
                continue;
            };
            let home = self.home.clone();
            let lease = attached.lease.clone();
            let renewed = tokio::task::spawn_blocking(move || {
                cw::shared::shared_blocking(&home)
                    .and_then(|s| s.renew(&lease, unix_now(), LEASE_TTL_SECS))
            })
            .await;
            if !matches!(renewed, Ok(Ok(true))) {
                entry.shared.workspace.lost.store(true, Ordering::Release);
                entry.control.stopped.store(true, Ordering::Release);
            }
        }
    }

    /// Resolve `spec` (`"new"` or a server id), take the lease, pin the
    /// image id and verify the mount source; fills `config`. Every failure
    /// after the lease was taken releases it again (CAS).
    pub(super) async fn attach_workspace(
        &self,
        agent_id: &str,
        spec: &str,
        session_id: &str,
        config: &mut ComputerUseConfig,
    ) -> Result<(AttachedWorkspace, WorkspacesConfig), OpError> {
        if spec != "new" && !paths::valid_workspace_id(spec) {
            return Err(ws_not_found());
        }
        let cfg = feature_config(&self.home, agent_id)?;
        let runner = self
            .workspace_rt
            .runner_id(&self.home)
            .await
            .ok_or_else(|| {
                OpError::new(
                    ErrorCode::WorkspaceUnavailable,
                    "無法確認這台主機的 Docker 環境（取不到 daemon id），工作區暫時不能掛載。",
                )
            })?;
        let store = cw::shared::shared_async(&self.home)
            .await
            .map_err(store_error)?;
        let id = if spec == "new" {
            // Under the new workspace's lock, on a blocking thread (the
            // lock may wait), so a reconciliation never sees a half-made one.
            let (home, owner, s, max, runner) = (
                self.home.clone(),
                agent_id.to_string(),
                store.clone(),
                cfg.max_per_agent,
                runner.clone(),
            );
            tokio::task::spawn_blocking(move || {
                cw::lock::create_workspace(&home, &s, &owner, &runner, unix_now(), max, &|_| {})
            })
            .await
            .map_err(|_| unavailable())?
            .map_err(store_error)?
        } else {
            let row = store.get_owned(spec, agent_id).map_err(store_error)?;
            if cw::orphan_if_owner_removed(&self.home, &store, &row) {
                return Err(state_error(WorkspaceState::Orphaned));
            }
            spec.to_string()
        };
        let created_note = |e: OpError| -> OpError {
            if spec == "new" {
                OpError::new(
                    e.code,
                    format!("{}（工作區 {id} 已建立，可稍後再掛載。）", e.message),
                )
            } else {
                e
            }
        };
        let (lease, _) = store
            .acquire(&AcquireRequest {
                workspace_id: &id,
                caller: agent_id,
                runner_id: &runner,
                holder: session_id,
                instance: instance_id(),
                now: unix_now(),
                ttl_secs: INITIAL_LEASE_TTL_SECS,
                retention_days: cfg.retention_days,
            })
            .map_err(|e| created_note(store_error(e)))?;
        let fail = |e: OpError| {
            let _ = store.release(&lease);
            created_note(e)
        };
        let image = match self.workspace_rt.image_id(&config.container_image).await {
            Ok(id) => id,
            Err(presence) => {
                let message =
                    crate::computer_use_image::presence_error(&config.container_image, presence)
                        .unwrap_or_default();
                return Err(fail(OpError::new(ErrorCode::Unavailable, message)));
            }
        };
        if store.set_image_digest(&lease, &image).is_err() {
            return Err(fail(lease_lost()));
        }
        let source = match paths::verify_mount_source(&self.home, &id) {
            Ok(source) => source,
            Err(why) => {
                let _ = store.note(
                    &id,
                    "mount_source_unsafe",
                    "system:attach",
                    json!({"reason": why.code()}),
                );
                return Err(fail(unavailable()));
            }
        };
        config.container_image = image.clone();
        config.workspace_mount = Some(WorkspaceMount {
            workspace_id: id,
            lease_epoch: lease.epoch,
            source,
        });
        Ok((
            AttachedWorkspace {
                lease,
                image_digest: image,
            },
            cfg,
        ))
    }

    /// The start answer's workspace fields.
    pub(super) fn workspace_fields(
        &self,
        attached: &AttachedWorkspace,
        cfg: &WorkspacesConfig,
        body: &mut Value,
    ) {
        let row = cw::shared::cached(&self.home)
            .and_then(|s| s.get(&attached.lease.workspace_id).ok().flatten());
        body["workspace_id"] = json!(attached.lease.workspace_id);
        body["mount_path"] = json!(paths::MOUNT_TARGET);
        body["quota"] = json!({"max_bytes": cfg.max_bytes, "max_files": cfg.max_files});
        if let Some(row) = row {
            body["data_revision"] = json!(row.data_revision);
            body["files_used"] = json!(row.files_used);
            body["bytes_used"] = json!(row.bytes_used);
            body["expires_at"] = json!(row.expires_at);
        }
    }
}

/// Map a backend start failure of a workspace session.
pub(crate) fn start_error_for_workspace(message: &str) -> Option<OpError> {
    if message == crate::computer_use_orchestrator::WORKSPACE_MOUNT_UNSAFE {
        return Some(unavailable());
    }
    if message == crate::computer_use_orchestrator::WORKSPACE_MOUNT_FAILED {
        return Some(OpError::new(ErrorCode::MountFailed, message));
    }
    None
}

/// The production Docker runtime.
pub(crate) fn docker_runtime() -> Arc<dyn WorkspaceRuntime> {
    Arc::new(DockerWorkspaceRuntime)
}
