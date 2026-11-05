//! Selection is a deployment decision, never a response to a failed spawn.
use std::sync::Arc;
use duduclaw_isolation::execution::{BackendError, ExecutionBackend, ExecutionClass};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallationMode { Local, ProtectedLinuxTeam }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind { Local, Spawnd }

pub const ISOLATION_UNAVAILABLE: &str =
    "受保護模式無法啟動員工：隔離服務或執行授權尚未就緒，請由管理員檢查服務狀態。";

pub fn required_backend(mode: InstallationMode, linux: bool) -> Result<BackendKind, BackendError> {
    match (mode, linux) {
        (InstallationMode::Local, _) => Ok(BackendKind::Local),
        (InstallationMode::ProtectedLinuxTeam, true) => Ok(BackendKind::Spawnd),
        (InstallationMode::ProtectedLinuxTeam, false) => Err(BackendError::Unavailable),
    }
}

/// Only the explicitly inventoried maintenance/probe classes may remain on the
/// gateway uid. A caller must supply the class from trusted routing, not input.
pub fn gateway_exception(class: ExecutionClass) -> bool {
    matches!(class, ExecutionClass::InstallationRequester | ExecutionClass::AccountProbe
        | ExecutionClass::ModelProbe | ExecutionClass::InteractiveLogin)
}

pub async fn select(mode: InstallationMode) -> Result<Arc<dyn ExecutionBackend>, BackendError> {
    match required_backend(mode, cfg!(target_os = "linux"))? {
        BackendKind::Local => Ok(Arc::new(crate::LocalBackend::default())),
        BackendKind::Spawnd => {
            #[cfg(target_os = "linux")]
            { Ok(Arc::new(crate::SpawndBackend::connect().await?)) }
            #[cfg(not(target_os = "linux"))]
            { Err(BackendError::Unavailable) }
        }
    }
}

/// Transitional callers with native child handles cannot represent a remote
/// run. Refuse them until their typed run/capability adapter is installed.
pub fn require_local(mode: InstallationMode, class: ExecutionClass) -> Result<(), BackendError> {
    if mode == InstallationMode::ProtectedLinuxTeam && !gateway_exception(class) {
        Err(BackendError::Unavailable)
    } else { Ok(()) }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn protected_selection_never_returns_local() {
        assert_eq!(required_backend(InstallationMode::ProtectedLinuxTeam, true), Ok(BackendKind::Spawnd));
        assert_eq!(required_backend(InstallationMode::ProtectedLinuxTeam, false), Err(BackendError::Unavailable));
        for linux in [false, true] {
            assert_eq!(required_backend(InstallationMode::Local, linux), Ok(BackendKind::Local));
        }
    }
    #[test]
    fn every_employee_and_worker_class_refuses_native_fallback() {
        use ExecutionClass::*;
        for class in [EmployeeMain, Scheduled, Dispatch, Heartbeat, Mail, Reminder,
            Judge, Fork, Discovery, EmployeeTool, ThirdPartyMcp, ToolLoopMcp, ParseWorker, ImportWorker] {
            assert_eq!(require_local(InstallationMode::ProtectedLinuxTeam, class), Err(BackendError::Unavailable));
            assert!(require_local(InstallationMode::Local, class).is_ok());
        }
        for class in [InstallationRequester, AccountProbe, ModelProbe, InteractiveLogin] {
            assert!(require_local(InstallationMode::ProtectedLinuxTeam, class).is_ok());
        }
    }
}
