//! HX5: gateway client for in-run helper calls (D8 revision §2.2, §2.5, §3).
//! A helper runs inside the caller's own live run. Every op travels on the
//! control connection that reserved that run, and only while that
//! connection's `Reserved.features` carries `helper_calls` [F17]. Nothing here
//! reserves, starts, waits on or cancels a run: the only stop is
//! `HelperCancel`/`HelperStop` of one `(run, call)` [F8].
use std::{os::fd::OwnedFd, sync::Arc, time::Duration};
use duduclaw_isolation::{execution::{BackendError, HelperError, HelperPlan, HelperStart, RunHandle, RuntimeKind},
    ids::EmployeeId, ipc::{self, ErrorCode, HelperCallId, HelperOutcome, Opcode, SpawnRequest, SpawnResponse}};
use super::{Entry, SpawndBackend, fd_io, transport::Packet};

/// `HelperWait` is a poll (HX4): `not_ready` until the call ended.
const WAIT_POLL: Duration = Duration::from_millis(100);

/// The per-call control ops that follow `HelperStarted`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Control { Wait, Stop, Cancel }

/// The ONLY place the gateway builds `HelperWait`/`HelperStop`/`HelperCancel`.
/// `employee` is the run's admitted employee (`Entry.employee`, from the
/// reservation), never caller data. Root refuses a request whose
/// `employee_id` is not the run's owner (HX4 security round, finding 8).
pub(super) fn control_request(control: Control, run: RunHandle, call: HelperCallId, employee: EmployeeId)
    -> SpawnRequest {
    let employee_id = employee;
    match control {
        Control::Wait => SpawnRequest::HelperWait { run, employee_id, call },
        Control::Stop => SpawnRequest::HelperStop { run, employee_id, call },
        Control::Cancel => SpawnRequest::HelperCancel { run, employee_id, call },
    }
}

/// Root's per-call refusal codes as typed results (HX §5). Never `map_error`:
/// that mapping loses `helper_busy` / `run_time_exhausted`.
pub(super) fn refusal(code: ErrorCode) -> HelperError {
    match code {
        ErrorCode::HelperBusy => HelperError::Busy,
        ErrorCode::RunTimeExhausted => HelperError::RunTimeExhausted,
        ErrorCode::HelperUnavailable => HelperError::Unavailable,
        ErrorCode::HelperCgroupUnavailable => HelperError::CgroupUnavailable,
        // Unknown run, or no longer Running (draining, stopping, ended).
        ErrorCode::NotFound | ErrorCode::NotReady => HelperError::NotLive,
        ErrorCode::Unauthorized => HelperError::Denied,
        // HX6: no such helper in the run's signed table (for the recording:
        // an offline parent, whose launcher carries no egress binding).
        ErrorCode::RuntimeDenied => HelperError::Absent,
        // HX6 security fix round (finding 4): spawnd's own recording gate.
        ErrorCode::HelperRecordingUnsupported => HelperError::RecordingUnsupported,
        _ => HelperError::Unavailable,
    }
}

impl SpawndBackend {
    /// The live entry of `run`, refusing locally (nothing sent) without the
    /// connection's `helper_calls` bit.
    fn helper_entry(&self, run: RunHandle) -> Result<Arc<Entry>, HelperError> {
        let entry = self.entry(run).map_err(|error| match error {
            BackendError::NotFound => HelperError::NotLive,
            _ => HelperError::Unavailable,
        })?;
        if entry.connection.is_closed() { return Err(HelperError::NotLive); }
        if !self.connection_features(run).allows(Opcode::HelperCall) { return Err(HelperError::FeatureMissing); }
        Ok(entry)
    }

    /// Start one helper in `plan.run`. The plan's employee must be the
    /// employee this gateway reserved the run for; root re-checks both.
    pub(super) async fn helper_start(&self, plan: HelperPlan) -> Result<HelperStart, HelperError> {
        let entry = self.helper_entry(plan.run)?;
        let input_bytes = u64::try_from(plan.input.len()).map_err(|_| HelperError::Denied)?;
        if entry.employee != plan.employee || !matches!(plan.helper, RuntimeKind::Registered(_))
            || input_bytes == 0 || input_bytes > ipc::MAX_HELPER_INPUT_BYTES || plan.output_limit == 0 {
            return Err(HelperError::Denied);
        }
        let memfd = sealed_input(&plan.input).map_err(|_| HelperError::Unavailable)?;
        let (run, timeout_ms) = (plan.run, plan.timeout_ms);
        let request = SpawnRequest::HelperCall { run, employee_id: plan.employee, helper: plan.helper,
            slot: plan.slot, timeout_ms, input_bytes };
        request.validate().map_err(|_| HelperError::Denied)?;
        let packet = helper_rpc(&entry, request, vec![memfd]).await?;
        match packet.response {
            // The transport already paired run, timeout and the two pipe roles.
            SpawnResponse::HelperStarted { run: actual, call, effective_timeout_ms }
                if actual == run && effective_timeout_ms <= timeout_ms => {
                match fd_io::make_helper_io(packet.fds, plan.output_limit) {
                    Ok((stdout, stderr)) => Ok(HelperStart { call, effective_timeout_ms, stdout, stderr }),
                    // Started but unreadable: kill only this call's slot.
                    Err(_) => {
                        let request = control_request(Control::Cancel, run, call, entry.employee);
                        let connection = entry.connection.clone();
                        tokio::spawn(async move { let _ = connection.helper_request(request, vec![]).await; });
                        Err(HelperError::Uncertain)
                    }
                }
            }
            SpawnResponse::Error { code } => Err(refusal(code)),
            _ => { entry.connection.close_now(); Err(HelperError::Uncertain) }
        }
    }

    /// Root-determined outcome of a started call. Polls `HelperWait`.
    pub(super) async fn helper_wait(&self, run: RunHandle, call: HelperCallId) -> Result<HelperOutcome, HelperError> {
        loop {
            let entry = self.helper_entry(run)?;
            let request = control_request(Control::Wait, run, call, entry.employee);
            let packet = helper_rpc(&entry, request, vec![]).await?;
            match packet.response {
                SpawnResponse::HelperEnded { outcome, .. } => return Ok(outcome),
                SpawnResponse::Error { code: ErrorCode::NotReady } => tokio::time::sleep(WAIT_POLL).await,
                SpawnResponse::Error { code } => return Err(refusal(code)),
                _ => { entry.connection.close_now(); return Err(HelperError::Uncertain); }
            }
        }
    }

    /// Kill the call's slot cgroup (queued or running), never the run.
    /// `Ok(())` is root's `Ack`: the stop/cancel was ACCEPTED, possibly while
    /// delivery is still pending (spawnd retries each tick, HX4 fix round). It
    /// never means the call ended; only `helper_wait`'s `HelperEnded` does.
    pub(super) async fn helper_control(&self, control: Control, run: RunHandle, call: HelperCallId)
        -> Result<(), HelperError> {
        if control == Control::Wait { return Err(HelperError::Denied); }
        let entry = self.helper_entry(run)?;
        let packet = helper_rpc(&entry, control_request(control, run, call, entry.employee), vec![]).await?;
        match packet.response {
            SpawnResponse::Ack => Ok(()),
            SpawnResponse::Error { code } => Err(refusal(code)),
            _ => { entry.connection.close_now(); Err(HelperError::Uncertain) }
        }
    }

}

/// Detached RPC. The transport closes a run's control connection when an
/// in-flight transaction is dropped, and root treats that as the gateway
/// leaving the run. An MCP cancel or revocation must kill only the helper, so
/// the transaction is owned by its own task and always runs to completion.
async fn helper_rpc(entry: &Arc<Entry>, request: SpawnRequest, fds: Vec<OwnedFd>) -> Result<Packet, HelperError> {
    let connection = entry.connection.clone();
    let task = tokio::spawn(async move { connection.helper_request(request, fds).await });
    task.await.map_err(|_| HelperError::Uncertain)?.map_err(|_| HelperError::Uncertain)
}

/// The sealed helper input memfd (`F_SEAL_SEAL|SHRINK|GROW|WRITE`, offset 0);
/// spawnd revalidates seals and the exact size before forwarding it.
pub(super) fn sealed_input(bytes: &[u8]) -> Result<OwnedFd, BackendError> {
    use std::io::{Seek, SeekFrom, Write};
    use std::os::fd::AsRawFd;
    use nix::fcntl::{fcntl, FcntlArg, SealFlag};
    let fd = crate::linux_syscalls::memfd_create(c"duduclaw-helper-input", libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING)
        .map_err(|_| BackendError::Io)?;
    let mut file = std::fs::File::from(fd);
    file.write_all(bytes).map_err(|_| BackendError::Io)?;
    file.seek(SeekFrom::Start(0)).map_err(|_| BackendError::Io)?;
    let seals = SealFlag::F_SEAL_SEAL | SealFlag::F_SEAL_SHRINK | SealFlag::F_SEAL_GROW | SealFlag::F_SEAL_WRITE;
    fcntl(file.as_raw_fd(), FcntlArg::F_ADD_SEALS(seals)).map_err(|_| BackendError::Io)?;
    let actual = fcntl(file.as_raw_fd(), FcntlArg::F_GET_SEALS).map_err(|_| BackendError::Io)?;
    if !SealFlag::from_bits_retain(actual).contains(seals) { return Err(BackendError::Io); }
    Ok(file.into())
}

#[cfg(test)]
#[path = "helper_call_tests.rs"]
mod tests;
