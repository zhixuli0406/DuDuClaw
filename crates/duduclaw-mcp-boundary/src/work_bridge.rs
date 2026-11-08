//! H5: the production [`WorkBroker`] — the boundary's `employee-work` root
//! over spawnd `OpenWorkFile` (D-H1). It lives here rather than in
//! `duduclaw-execution` because this crate already depends on that one (the
//! trait is defined here); the spawnd client itself is
//! `duduclaw_execution::SpawndBackend::open_work_file`.
//!
//! Run and employee come only from the [`WorkFileRequest`], which the boundary
//! builds from the verified capability record (H6); they are forwarded
//! unchanged on the run's own authenticated control connection. Nothing is
//! sent unless spawnd advertises `work_broker`. The received fd is the sealed
//! read-only memfd snapshot root made (H2); its metadata is reported as read
//! (`nlink` 0), and the boundary's `verify_brokered` checks it again.
use std::sync::Arc;
use duduclaw_execution::{spawnd::WorkFileError, SpawndBackend};
use duduclaw_isolation::{execution::RunHandle, ipc::ControlConnectionId};
use crate::resources::{BrokeredWorkFile, WorkBroker, WorkBrokerError, WorkFileMetadata, WorkFileRequest};

pub struct SpawndWorkspaceBridge { backend: Arc<SpawndBackend> }

impl SpawndWorkspaceBridge {
    /// `backend` must be the protected backend that reserved the employee's
    /// runs (the control connection that owns a run is the only one root
    /// accepts for its work ops). Wire with `ResourceRoots::insert_brokered`.
    pub fn new(backend: Arc<SpawndBackend>) -> Self { Self { backend } }

    /// W1: [`WorkBroker::open_work_file`] only while `request.run()` is still
    /// reserved on the control connection `control` (the one a run-bound
    /// broker latched); otherwise `NotLive` and nothing is sent.
    pub fn open_on_control(&self, request: &WorkFileRequest<'_>, control: ControlConnectionId)
        -> Result<BrokeredWorkFile, WorkBrokerError> {
        brokered(self.backend.open_work_file_on(RunHandle(request.run()), Some(control), request.employee(),
            request.path().as_str(), request.max_bytes()))
    }
}

impl WorkBroker for SpawndWorkspaceBridge {
    fn open_work_file(&self, request: &WorkFileRequest<'_>) -> Result<BrokeredWorkFile, WorkBrokerError> {
        brokered(self.backend.open_work_file(RunHandle(request.run()), request.employee(),
            request.path().as_str(), request.max_bytes()))
    }
}

fn brokered(result: Result<duduclaw_execution::spawnd::WorkFileSnapshot, WorkFileError>)
    -> Result<BrokeredWorkFile, WorkBrokerError> {
    let snapshot = result.map_err(broker_error)?;
    Ok(BrokeredWorkFile { file: snapshot.file, metadata: WorkFileMetadata { regular: true,
        size: snapshot.size, device: snapshot.device, inode: snapshot.inode, nlink: snapshot.nlink } })
}

/// spawnd refusal → boundary diagnostic (→ `WorkBrokerError::user_message`).
pub fn broker_error(error: WorkFileError) -> WorkBrokerError {
    match error {
        WorkFileError::NotLive => WorkBrokerError::NotLive,
        WorkFileError::NotFound => WorkBrokerError::NotFound,
        WorkFileError::Refused => WorkBrokerError::Refused,
        WorkFileError::TooLarge => WorkBrokerError::TooLarge,
        WorkFileError::BrokerUnavailable | WorkFileError::Unavailable => WorkBrokerError::Unavailable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn h5_bridge_maps_spawnd_refusals() {
        for (error, expected) in [(WorkFileError::NotLive, WorkBrokerError::NotLive),
            (WorkFileError::NotFound, WorkBrokerError::NotFound), (WorkFileError::Refused, WorkBrokerError::Refused),
            (WorkFileError::TooLarge, WorkBrokerError::TooLarge),
            (WorkFileError::BrokerUnavailable, WorkBrokerError::Unavailable),
            (WorkFileError::Unavailable, WorkBrokerError::Unavailable)] {
            assert_eq!(broker_error(error), expected, "{error:?}");
        }
        // A missing feature bit reads as the readiness reason H16 reports.
        assert_eq!(broker_error(WorkFileError::BrokerUnavailable).code(), "workspace_broker_unavailable");
    }
}
