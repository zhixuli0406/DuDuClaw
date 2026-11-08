//! HX6 (D8 revision §4): `browser_record_start/stop` as an in-run helper in
//! the recording slot of the caller's own run [F7]. Start returns once spawnd
//! reports `HelperStarted`; a gateway collector then owns the call's pipes,
//! waits for `HelperEnded` and stores the strictly decoded driver result
//! (with artifact sha256 [F11]) in the `(run, call)` recording store [F8].
//!
//! - Stop in the same run: the old `:180` refusal is inverted — a live
//!   recording of the caller's own run is the allowed case, resolved through
//!   the `(run, call)` table (never a run handle) → `HelperStop` → the
//!   collector's stored result is returned synchronously.
//! - Stop in a later run, or of a terminal row: the stored result, no helper
//!   interaction.
//! - Run end without stop: the launcher flushes; the result is stored with
//!   `ended_by: run_end`. Hard kill before flush: `uncertain/run_killed`;
//!   artifacts are never fabricated.
//! - Revocation / principal-generation change [F16]: `HelperStop`, stored
//!   with `ended_by: revoked`.
//!
//! No path here reserves, spawns, waits on or kills a run: the only controls
//! are `HelperStop`/`HelperCancel` of one `(run, call)`.
use std::{sync::Arc, time::Duration};
use duduclaw_isolation::{execution::{ExecutionBackend, HelperError, HelperStart, RunHandle, RunReader},
    ipc::{HelperOutcome, HelperSlot, SpawndFeatures}};
use serde_json::{Value, json};
use crate::{RequestContext, VerificationError, tool_policy,
    recording_jobs::{parse_id, RecordingJobs, RecordingKey, RecordingRow, StopBy}};
use super::{ExecutionPlanFactory, ExecutionToolBridge, in_run, read_result, refused, result_envelope, uncertain};

const START: &str = "browser_record_start";
const STOP: &str = "browser_record_stop";
/// The driver result bound (`recording_helper::MAX_RESULT`).
const RESULT_LIMIT: usize = 64 * 1024;
const STDERR_LIMIT: usize = 64 * 1024;
const CONTROL_TIMEOUT: Duration = Duration::from_secs(5);
/// How long a stop waits for the flush (trace + artifact publication). Past
/// it the caller gets `stop_requested` and reads the result with a repeat.
const STOP_WAIT: Duration = Duration::from_secs(60);
const STORE_POLL: Duration = Duration::from_millis(100);
/// Collector bound beyond the effective timeout: launcher flush window,
/// spawnd watchdog, slot drain. Root enforces the real deadline.
const COLLECT_SLACK: Duration = Duration::from_secs(60);

pub(super) fn is_recording(name: &str) -> bool { tool_policy::IN_RUN_RECORDING_TOOLS.contains(&name) }

impl ExecutionToolBridge {
    pub(super) async fn execute_recording(&self, context: &RequestContext, name: &str, arguments: &Value) -> Value {
        let Some(store) = self.recordings.clone() else { return refused(); };
        if !self.supports(name) { return refused(); }
        if name == START { self.record_start(store, context, arguments).await }
        else { self.record_stop(store, context, arguments).await }
    }

    /// The recording bit is a host availability gate (Chromium proven to start
    /// under the helper sandbox); it rides on the run's own admission bits.
    fn recording_supported(&self, run: RunHandle) -> bool {
        self.backend.helper_features(run).contains(SpawndFeatures::HELPER_CALLS.with(SpawndFeatures::HELPER_RECORDING))
    }

    async fn record_start(&self, store: Arc<dyn RecordingJobs>, context: &RequestContext, arguments: &Value) -> Value {
        // [F7] Only the run's own MCP transport may start a helper in it.
        let record_run = context.record().run;
        if context.transport_run() != Some(record_run) { return refused(); }
        let run = RunHandle(record_run);
        let call = match tool_policy::authorize_async(context, START, arguments, self.resolver.clone()).await {
            Ok(call) => call, Err(_) => return refused(),
        };
        if !self.helper_calls(run) { return in_run::helper_refusal(HelperError::FeatureMissing); }
        if !self.recording_supported(run) { return in_run::typed("helper_recording_unsupported"); }
        if self.factory.authorize_handler(&call).await.is_err() { return refused(); }
        let plan = match self.factory.prepare_in_run(&call).await {
            Ok(plan) if plan.run() == run && plan.employee() == context.record().employee
                && plan.slot() == HelperSlot::Recording => plan,
            _ => return refused(),
        };
        let checked = match tool_policy::authorize_async(context, START, arguments, self.resolver.clone()).await {
            Ok(checked) if checked.resources() == call.resources() => checked,
            _ => return refused(),
        };
        if self.factory.authorize_handler(&checked).await.is_err() { return refused(); }
        let live = checked.context().clone();
        if crate::authority::verify_blocking(move || live.verify_live(crate::authority::now_seconds()?)).await.is_err() {
            return refused();
        }
        if !self.recording_supported(run) { return in_run::typed("helper_recording_unsupported"); }
        let limit = usize::try_from(plan.output_limit()).unwrap_or(usize::MAX).min(RESULT_LIMIT + 1);
        let HelperStart { call: id, effective_timeout_ms, stdout, stderr } =
            match self.backend.start_helper(plan.into_plan()).await {
                Ok(started) => started,
                // No recording binding in this run's signed table: by
                // publication check 4 its parent is not an Egress profile.
                Err(HelperError::Absent) => return in_run::typed("helper_parent_offline"),
                Err(error) => return in_run::helper_refusal(error),
            };
        let key = RecordingKey { run, call: id };
        // Armed until the collector owns the call: a dropped request (MCP
        // cancel, connection close) must not leave an unowned recording.
        let mut guard = StartGuard { backend: self.backend.clone(), store: store.clone(), key: Some(key) };
        let (insert_store, owner) = (store.clone(), checked.context().clone());
        if crate::authority::verify_blocking(move || insert_store.insert(&owner, key)).await.is_err() {
            guard.abandon();
            return refused();
        }
        guard.key = None;
        let collector = Collector { backend: self.backend.clone(), factory: self.factory.clone(), store, key,
            context: checked.context().clone(),
            bound: Duration::from_millis(effective_timeout_ms).saturating_add(COLLECT_SLACK) };
        tokio::spawn(collector.run(stdout, stderr, limit));
        result_envelope(json!({"status":"recording", "id":key.id(), "effective_timeout_ms":effective_timeout_ms}))
    }

    async fn record_stop(&self, store: Arc<dyn RecordingJobs>, context: &RequestContext, arguments: &Value) -> Value {
        if crate::check_arguments(arguments).is_err() { return refused(); }
        let Some(fields) = arguments.as_object() else { return refused(); };
        if fields.keys().any(|key| !matches!(key.as_str(), "id" | "workspace")) { return refused(); }
        let Some(call) = arguments["id"].as_str().and_then(|id| parse_id(id).ok()) else { return refused(); };
        let before = match tool_policy::authorize_async(context, STOP, arguments, self.resolver.clone()).await {
            Ok(call) => call, Err(_) => return refused(),
        };
        if self.factory.authorize_control(context, STOP, arguments).await.is_err() { return refused(); }
        let after = match tool_policy::authorize_async(context, STOP, arguments, self.resolver.clone()).await {
            Ok(call) => call, Err(_) => return refused(),
        };
        if before.resources() != after.resources() { return refused(); }
        let (reader, owner) = (store.clone(), context.clone());
        let row = match crate::authority::verify_blocking(move || reader.read(&owner, call)).await {
            Ok(row) => row, Err(_) => return refused(),
        };
        // Terminal, or a live recording of another (earlier) run: the stored
        // state only. A later run never reaches the helper of another run.
        if row.state.terminal() || row.key.run.0 != context.record().run { return result_envelope(row.view()); }
        // [F8] Same run: the fixed `:180` inversion. Only this run's own MCP
        // transport may stop its helper.
        if context.transport_run() != Some(context.record().run) { return refused(); }
        let (writer, owner, key) = (store.clone(), context.clone(), row.key);
        if crate::authority::verify_blocking(move || writer.request_stop(&owner, key)).await.is_err() { return refused(); }
        match tokio::time::timeout(CONTROL_TIMEOUT, self.backend.stop_helper(key.run, key.call)).await {
            // NotLive: the run ended meanwhile; the collector stores its flush.
            Ok(Ok(())) | Ok(Err(HelperError::NotLive)) => {}
            _ => return uncertain(),
        }
        match await_terminal(store, row).await { Some(row) => result_envelope(row.view()), None => uncertain() }
    }
}

/// HX6 Admin RPC (`isolation.recording_status/stop`), after the gateway
/// re-checked the dashboard Admin role. Live run → `HelperStop` for that
/// `(run, call)`; ended → the stored result. Admin never issues `HelperCall`
/// and never mints an employee context.
pub async fn control_recording(store: Arc<dyn RecordingJobs>, backend: Arc<dyn ExecutionBackend>, id: &str, stop: bool)
    -> Result<Value, VerificationError> {
    let call = parse_id(id)?;
    let reader = store.clone();
    let row = crate::authority::verify_blocking(move || reader.admin_read(call)).await?;
    if !stop || row.state.terminal() { return Ok(row.view()); }
    let (writer, key) = (store.clone(), row.key);
    crate::authority::verify_blocking(move || writer.mark_stop(key, StopBy::Admin)).await?;
    match tokio::time::timeout(CONTROL_TIMEOUT, backend.stop_helper(key.run, key.call)).await {
        Ok(Ok(())) | Ok(Err(HelperError::NotLive)) => {}
        _ => return Err(VerificationError::Unavailable),
    }
    await_terminal(store, row).await.map(|row| row.view()).ok_or(VerificationError::Unavailable)
}

/// The row once terminal, or as last read when the flush outlasts `STOP_WAIT`.
async fn await_terminal(store: Arc<dyn RecordingJobs>, mut row: RecordingRow) -> Option<RecordingRow> {
    let deadline = tokio::time::Instant::now() + STOP_WAIT;
    while !row.state.terminal() && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(STORE_POLL).await;
        let (reader, call) = (store.clone(), row.key.call);
        row = crate::authority::verify_blocking(move || reader.admin_read(call)).await.ok()?;
    }
    Some(row)
}

/// Before the collector owns a started call: on drop (or `abandon`) cancel the
/// slot and publish uncertainty (if the row exists). Never touches the run.
struct StartGuard { backend: Arc<dyn ExecutionBackend>, store: Arc<dyn RecordingJobs>, key: Option<RecordingKey> }
impl StartGuard {
    fn abandon(&mut self) {
        let Some(key) = self.key.take() else { return };
        let (backend, store) = (self.backend.clone(), self.store.clone());
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                let _ = tokio::time::timeout(CONTROL_TIMEOUT, backend.cancel_helper(key.run, key.call)).await;
                let _ = crate::authority::verify_blocking(move ||
                    store.finish(key, json!({"status":"uncertain","reason":"start_abandoned"}))).await;
            });
        }
    }
}
impl Drop for StartGuard { fn drop(&mut self) { self.abandon(); } }

struct Collector {
    backend: Arc<dyn ExecutionBackend>,
    factory: Arc<dyn ExecutionPlanFactory>,
    store: Arc<dyn RecordingJobs>,
    key: RecordingKey,
    /// The start request's verified context: its live check is the revocation signal.
    context: RequestContext,
    bound: Duration,
}

/// What the collector observed of the call: bounded output (if the pipes
/// reached EOF within bounds) and root's end.
type Ended = (Option<(Vec<u8>, Vec<u8>)>, Result<HelperOutcome, HelperError>);

impl Collector {
    async fn run(self, stdout: Box<dyn RunReader>, stderr: Box<dyn RunReader>, limit: usize) {
        let (backend, key) = (self.backend.clone(), self.key);
        let observed = async move {
            let output = tokio::try_join!(read_result(stdout, limit), read_result(stderr, STDERR_LIMIT)).ok();
            (output, backend.wait_helper(key.run, key.call).await)
        };
        tokio::pin!(observed);
        let bound = tokio::time::sleep(self.bound);
        tokio::pin!(bound);
        let mut revocation_handled = false;
        let ended: Option<Ended> = loop {
            tokio::select! {
                biased;
                ended = &mut observed => break Some(ended),
                () = &mut bound => break None,
                () = in_run::revoked(self.context.clone()), if !revocation_handled => {
                    revocation_handled = true;
                    self.revoke().await;
                }
            }
        };
        let result = match ended {
            Some(ended) => self.result(ended).await,
            None => {
                // A broken transport or a stuck root: kill only the slot.
                let _ = tokio::time::timeout(CONTROL_TIMEOUT, self.backend.cancel_helper(key.run, key.call)).await;
                json!({"status":"uncertain", "reason":"collector_timeout"})
            }
        };
        let store = self.store.clone();
        let _ = crate::authority::verify_blocking(move || store.finish(key, result)).await;
    }

    /// [F16] Live authority failed. A run that already ended answers
    /// `NotLive`: that is run end, not revocation (the outcome says so).
    async fn revoke(&self) {
        let key = self.key;
        if let Ok(Ok(())) = tokio::time::timeout(CONTROL_TIMEOUT, self.backend.stop_helper(key.run, key.call)).await {
            let store = self.store.clone();
            let _ = crate::authority::verify_blocking(move || store.mark_stop(key, StopBy::Revoked)).await;
        }
    }

    async fn result(&self, (output, outcome): Ended) -> Value {
        // `NotLive` from `HelperWait`: spawnd dropped the run with the call.
        let outcome = match outcome {
            Ok(outcome) => outcome,
            Err(HelperError::NotLive) => HelperOutcome::RunEnded,
            Err(_) => return json!({"status":"uncertain", "reason":"helper_unavailable"}),
        };
        let decoded = output.as_ref().and_then(|(stdout, stderr)|
            self.factory.decode_helper_result(START, stdout, stderr, &outcome).ok())
            .filter(|value| value["id"] == self.key.id());
        let Some(mut value) = decoded else {
            return match outcome {
                // Exited cleanly but the bytes do not decode: never stored as a recording.
                HelperOutcome::Exited(0) => json!({"status":"invalid_result"}),
                // EX_CONFIG: the helper could not install the browser's nested
                // sandbox on this host (Landlock); it never starts Chromium unconfined.
                HelperOutcome::Exited(78) => json!({"status":"uncertain", "reason":"helper_recording_unsupported"}),
                HelperOutcome::Exited(code) => json!({"status":"uncertain", "reason":"helper_failed", "exit_code":code}),
                HelperOutcome::TimedOut => json!({"status":"uncertain", "reason":"helper_timed_out"}),
                HelperOutcome::OomKilled => json!({"status":"uncertain", "reason":"helper_oom_killed"}),
                HelperOutcome::StartFailed => json!({"status":"uncertain", "reason":"helper_unavailable"}),
                // Killed before the flush: artifacts are never fabricated.
                _ => json!({"status":"uncertain", "reason":"run_killed"}),
            };
        };
        let (store, key) = (self.store.clone(), self.key);
        let requested = crate::authority::verify_blocking(move || store.stop_by(key)).await.ok().flatten();
        let ended_by = match requested {
            Some(by) => by.as_str(),
            None if matches!(outcome, HelperOutcome::RunEnded) => "run_end",
            None if value["reason"] == "deadline" => "deadline",
            // `stop` that nobody here asked for is the launcher's drain at run end.
            None => "run_end",
        };
        if let Some(object) = value.as_object_mut() { object.insert("ended_by".into(), json!(ended_by)); }
        value
    }
}

#[cfg(all(test, feature = "binding-store"))]
#[path = "execution_recording_tests.rs"]
mod tests;
