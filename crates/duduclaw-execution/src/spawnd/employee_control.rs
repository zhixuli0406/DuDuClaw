//! F6d: gateway client for the two employee-level H4 requests (F6 design
//! §3.2 S13, §3.4 M8): `StopEmployeeRuns` (tombstone: cancel every live run
//! of the employee and wait for the confirmed stop) and `EmployeeIdle` (the
//! read-only liveness readback a restore needs).
//!
//! Neither belongs to a run, so each call opens its own authenticated control
//! connection (`transport::connect`: root-owned socket, SO_PEERCRED root) and
//! closes it afterwards; a run's connection is never borrowed. An older
//! spawnd that cannot decode the op drops this throwaway connection only.
//!
//! **Fail closed.** Any transport error, timeout, malformed or unpaired
//! reply is [`EmployeeControlError::Unavailable`] — never "gone" and never
//! "idle". Root's own `NotFound` is reported as such; the caller decides
//! (the restore precheck treats it as inconsistent, not as an authoritative
//! absence: that comes only from the allocation ReadBack, S5b).
use std::time::Duration;
use duduclaw_isolation::{execution::BackendError, ids::EmployeeId, ipc::{self, EmployeeIdleReport, ErrorCode, SpawnRequest, SpawnResponse}};
use super::transport;

/// Root answers both within `BROKER_REPLY_MS` (4 s; the stop waits ≤ 3.5 s
/// for the confirmation). One second of slack for the socket round trip.
const EMPLOYEE_CONTROL_TIMEOUT: Duration = Duration::from_secs(5);

/// Typed refusal of an employee-level control call. Carries no root detail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmployeeControlError {
    /// Transport failure, timeout, no or malformed reply, or root answered
    /// `IsolationUnavailable` / `NotReady` / `WorkspaceBrokerUnavailable`.
    Unavailable,
    /// Root reported `NotFound` for the employee.
    NotFound,
    /// Any other root refusal (`Unauthorized`, `InvalidRequest`, …).
    Refused,
}

/// A confirmed or unconfirmed stop: the run ids root cancelled (or found
/// already stopping) and whether `EmployeeIdle` read idle afterwards.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmployeeRunsStopped {
    pub runs: Vec<[u8; 16]>,
    pub confirmed: bool,
}

fn exchange(request: SpawnRequest) -> Result<SpawnResponse, EmployeeControlError> {
    let connection = transport::connect().map_err(|_| EmployeeControlError::Unavailable)?;
    let packet = connection.work_request(request.clone(), EMPLOYEE_CONTROL_TIMEOUT)
        .map_err(|_: BackendError| EmployeeControlError::Unavailable)?;
    // Paired again here (the transport already did): exact employee, no fd.
    if ipc::check_work_reply(&request, &packet.response, packet.fds.len()).is_err() || !packet.fds.is_empty() {
        connection.close_now();
        return Err(EmployeeControlError::Unavailable);
    }
    connection.close_now();
    Ok(packet.response)
}

fn refusal(code: ErrorCode) -> EmployeeControlError {
    match code {
        ErrorCode::NotFound => EmployeeControlError::NotFound,
        ErrorCode::IsolationUnavailable | ErrorCode::NotReady | ErrorCode::WorkspaceBrokerUnavailable
            | ErrorCode::QueueFull | ErrorCode::QueueExpired | ErrorCode::ResourceLimit => EmployeeControlError::Unavailable,
        _ => EmployeeControlError::Refused,
    }
}

/// H4 `EmployeeIdle`: runs, attachments, processes and units of `employee`.
/// Blocking (≤ 5 s): blocking pool only.
pub fn employee_idle(employee: EmployeeId) -> Result<EmployeeIdleReport, EmployeeControlError> {
    match exchange(SpawnRequest::EmployeeIdle { employee_id: employee })? {
        SpawnResponse::EmployeeIdle { employee_id, report } if employee_id == employee => Ok(report),
        SpawnResponse::Error { code } => Err(refusal(code)),
        _ => Err(EmployeeControlError::Unavailable),
    }
}

/// H4 `StopEmployeeRuns`: cancel every live run of `employee` and wait
/// (bounded, root side) for the confirmed stop. Blocking (≤ 5 s).
pub fn stop_employee_runs(employee: EmployeeId) -> Result<EmployeeRunsStopped, EmployeeControlError> {
    match exchange(SpawnRequest::StopEmployeeRuns { employee_id: employee })? {
        SpawnResponse::EmployeeRunsStopped { employee_id, runs, confirmed } if employee_id == employee =>
            Ok(EmployeeRunsStopped { runs: runs.as_slice().iter().map(|run| run.0 .0).collect(), confirmed }),
        SpawnResponse::Error { code } => Err(refusal(code)),
        _ => Err(EmployeeControlError::Unavailable),
    }
}
