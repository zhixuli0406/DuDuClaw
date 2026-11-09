//! HX5 (D8 revision §3): synchronous in-run helper calls for `execute_program`,
//! `code_map` and `office_script`. The helper runs inside the caller's own run,
//! the run whose MCP gateway transport carried the call [F7]. Nothing is
//! reserved, no job row is written, and the legacy host execution path (child
//! reservation, `kill(run)`, `kill_authorized(run)`) is unreachable for these
//! tools [F8]. The call blocks until spawnd reports the helper's end.
use std::{sync::Arc, time::Duration};
use duduclaw_isolation::{execution::{ExecutionBackend, HelperError, HelperPlan, HelperStart, RunHandle, RuntimeKind},
    ids::EmployeeId, ipc::{self, HelperCallId, HelperOutcome, HelperSlot, SpawndFeatures}};
use serde_json::{Value, json};
use crate::{RequestContext, VerificationError, tool_policy::{self, Route}};
use super::{ExecutionToolBridge, read_result, refused, uncertain};

/// Decoders require an empty stderr; anything beyond this is a protocol error.
const STDERR_LIMIT: usize = 64 * 1024;
/// Gateway slack beyond spawnd's effective timeout: its watchdog (+2 s), the
/// slot drain and result delivery. 300 s + 4 s stays below the 305 s CLI tool
/// timeout (`config::CLI_TOOL_TIMEOUT_MS`), so the runtime sees our result.
const RESULT_SLACK_MS: u64 = 4_000;
/// Revocation / principal-generation changes are observed at this cadence.
const LIVE_POLL: Duration = Duration::from_millis(250);
const CANCEL_TIMEOUT: Duration = Duration::from_secs(5);
/// Bound on collecting `HelperEnded` after an accepted cancel (background).
const END_AFTER_CANCEL: Duration = Duration::from_secs(30);

/// The foreground in-run tools. The HX6 recording tools share
/// `Route::InRunHelper` but use the recording slot (`execution_recording.rs`).
pub(super) fn is_in_run(name: &str) -> bool {
    tool_policy::policy(name).is_some_and(|policy| policy.route == Route::InRunHelper)
        && tool_policy::IN_RUN_HELPER_TOOLS.contains(&name)
}

/// Same-run helper plan from the trusted plan factory (§3 step 2). The bridge
/// still requires `run == record.run == transport run` and the record's
/// employee before anything is sent; helpers are registered runtimes only.
pub struct AuthorizedHelperPlan {
    run: RunHandle,
    employee: EmployeeId,
    helper: RuntimeKind,
    timeout_ms: u64,
    input: Vec<u8>,
    output_limit: u64,
    slot: HelperSlot,
}
impl AuthorizedHelperPlan {
    /// A foreground-slot plan (`execute_program`, `code_map`, `office_script`).
    pub fn new(run: RunHandle, employee: EmployeeId, helper: RuntimeKind, timeout_ms: u64,
        input: Vec<u8>, output_limit: u64) -> Result<Self, VerificationError> {
        Self::with_slot(run, employee, helper, timeout_ms, input, output_limit, HelperSlot::Foreground)
    }
    /// HX6: a recording-slot plan (`browser_record_start`).
    pub fn recording(run: RunHandle, employee: EmployeeId, helper: RuntimeKind, timeout_ms: u64,
        input: Vec<u8>, output_limit: u64) -> Result<Self, VerificationError> {
        Self::with_slot(run, employee, helper, timeout_ms, input, output_limit, HelperSlot::Recording)
    }
    fn with_slot(run: RunHandle, employee: EmployeeId, helper: RuntimeKind, timeout_ms: u64,
        input: Vec<u8>, output_limit: u64, slot: HelperSlot) -> Result<Self, VerificationError> {
        if !matches!(helper, RuntimeKind::Registered(_)) || timeout_ms == 0
            || timeout_ms > ipc::MAX_HELPER_TIMEOUT_MS || input.is_empty()
            || input.len() as u64 > ipc::MAX_HELPER_INPUT_BYTES || output_limit == 0 {
            return Err(VerificationError::Invalid);
        }
        Ok(Self { run, employee, helper, timeout_ms, input, output_limit, slot })
    }
    pub fn run(&self) -> RunHandle { self.run }
    pub fn timeout_ms(&self) -> u64 { self.timeout_ms }
    pub fn slot(&self) -> HelperSlot { self.slot }
    pub(super) fn employee(&self) -> EmployeeId { self.employee }
    pub(super) fn output_limit(&self) -> u64 { self.output_limit }
    pub(super) fn into_plan(self) -> HelperPlan {
        HelperPlan { run: self.run, employee: self.employee, helper: self.helper, slot: self.slot,
            timeout_ms: self.timeout_ms, input: self.input, output_limit: self.output_limit }
    }
}

/// [F8] An in-run job is keyed `(run, call)` in its own namespace, separate
/// from run handles. It exposes no `RunHandle`, so it can never be passed to
/// the run-killing `kill`/`kill_authorized`; its only stop is `HelperCancel`.
pub struct InRunJob { run: RunHandle, call: HelperCallId }
impl InRunJob {
    pub fn call(&self) -> HelperCallId { self.call }
    /// `<run hex>:<call hex>`, for logs and audit; not a bearer selector.
    pub fn key(&self) -> String { format!("{}:{}", crate::execution_jobs::handle_text(self.run), self.call.hex()) }
    /// Send `HelperCancel`. Its `Ack` only means root ACCEPTED the cancel:
    /// delivery to the helper may still be pending (spawnd retries each tick),
    /// so it is never treated as the call's end. The end is `HelperEnded`
    /// alone; it is collected in the background (bounded) so the request that
    /// cancelled answers refused/uncertain — never "ended" — without waiting
    /// past its own deadline.
    async fn cancel(self, backend: Arc<dyn ExecutionBackend>) {
        let accepted = matches!(tokio::time::timeout(CANCEL_TIMEOUT, backend.cancel_helper(self.run, self.call)).await,
            Ok(Ok(())));
        if !accepted { return; }
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(collect_end(backend, self.run, self.call));
        }
    }
}

/// After an accepted cancel: poll `HelperWait` until root reports `HelperEnded`
/// (consuming root's ended record), the run is gone, or the bound expires.
async fn collect_end(backend: Arc<dyn ExecutionBackend>, run: RunHandle, call: HelperCallId) -> Option<HelperOutcome> {
    tokio::time::timeout(END_AFTER_CANCEL, backend.wait_helper(run, call)).await.ok()?.ok()
}

/// Armed from `HelperStarted` until `HelperEnded`. Dropping the call future —
/// MCP `notifications/cancelled`, connection close, revocation of the
/// connection or the request deadline (server.rs) — sends `HelperCancel` [Q5].
struct CallGuard { backend: Arc<dyn ExecutionBackend>, job: Option<InRunJob> }
impl CallGuard {
    fn disarm(&mut self) { self.job = None; }
    async fn cancel(&mut self) {
        if let Some(job) = self.job.take() { job.cancel(self.backend.clone()).await; }
    }
}
impl Drop for CallGuard {
    fn drop(&mut self) {
        let Some(job) = self.job.take() else { return };
        let backend = self.backend.clone();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(job.cancel(backend));
        }
    }
}

enum Observed { Done(Vec<u8>, Vec<u8>, HelperOutcome), Helper(HelperError), Uncertain, Revoked, TimedOut }

impl ExecutionToolBridge {
    pub(super) async fn execute_in_run(&self, context: &RequestContext, name: &str, arguments: &Value) -> Value {
        // [F7] Only the run's own MCP transport may start a helper in it.
        // Delegated, Admin-RPC and record-only contexts carry no transport run.
        let record_run = context.record().run;
        if context.transport_run() != Some(record_run) || !self.supports(name) { return refused(); }
        let run = RunHandle(record_run);
        let call = match tool_policy::authorize_async(context, name, arguments, self.resolver.clone()).await {
            Ok(call) => call, Err(_) => return refused(),
        };
        // [F17] Never send a helper op to a spawnd that did not advertise it.
        if !self.helper_calls(run) { return helper_refusal(HelperError::FeatureMissing); }
        if self.factory.authorize_handler(&call).await.is_err() { return refused(); }
        // The target must EQUAL the caller's capability run (the inverse of
        // the child-run rule) and its employee.
        let plan = match self.factory.prepare_in_run(&call).await {
            Ok(plan) if plan.run == run && plan.employee == context.record().employee
                && plan.slot == HelperSlot::Foreground => plan,
            _ => return refused(),
        };
        // Re-authorize after preparation; verify_live immediately before send.
        let checked = match tool_policy::authorize_async(context, name, arguments, self.resolver.clone()).await {
            Ok(checked) if checked.resources() == call.resources() => checked,
            _ => return refused(),
        };
        if self.factory.authorize_handler(&checked).await.is_err() { return refused(); }
        let live = checked.context().clone();
        if crate::authority::verify_blocking(move || live.verify_live(crate::authority::now_seconds()?)).await.is_err() {
            return refused();
        }
        if !self.helper_calls(run) { return helper_refusal(HelperError::FeatureMissing); }
        let output_limit = usize::try_from(plan.output_limit).unwrap_or(usize::MAX);
        let HelperStart { call: id, effective_timeout_ms, stdout, stderr } =
            match self.backend.start_helper(plan.into_plan()).await {
                Ok(started) => started, Err(error) => return helper_refusal(error),
            };
        let mut guard = CallGuard { backend: self.backend.clone(), job: Some(InRunJob { run, call: id }) };
        let deadline = Duration::from_millis(effective_timeout_ms.saturating_add(RESULT_SLACK_MS));
        let backend = self.backend.clone();
        let completion = async move {
            let output = tokio::try_join!(read_result(stdout, output_limit), read_result(stderr, STDERR_LIMIT));
            let Ok((stdout, stderr)) = output else { return Observed::Uncertain; };
            match backend.wait_helper(run, id).await {
                Ok(outcome) => Observed::Done(stdout, stderr, outcome),
                Err(error) => Observed::Helper(error),
            }
        };
        let observed = tokio::select! {
            biased;
            () = revoked(checked.context().clone()) => Observed::Revoked,
            result = tokio::time::timeout(deadline, completion) => result.unwrap_or(Observed::TimedOut),
        };
        match observed {
            Observed::Done(stdout, stderr, outcome) => {
                guard.disarm();
                self.finish(name, &stdout, &stderr, outcome, effective_timeout_ms)
            }
            // spawnd lost the run between start and wait: nothing is retried.
            Observed::Helper(HelperError::NotLive) => { guard.disarm(); typed("run_ended") }
            // Capability revoked or principal generation changed mid-call:
            // kill the helper's slot (never the run) and refuse like today's
            // cancelled path.
            Observed::Revoked => { guard.cancel().await; refused() }
            Observed::Helper(_) | Observed::Uncertain | Observed::TimedOut => { guard.cancel().await; uncertain() }
        }
    }

    pub(super) fn helper_calls(&self, run: RunHandle) -> bool {
        self.backend.helper_features(run).contains(SpawndFeatures::HELPER_CALLS)
    }

    fn finish(&self, name: &str, stdout: &[u8], stderr: &[u8], outcome: HelperOutcome, effective_timeout_ms: u64) -> Value {
        match outcome {
            HelperOutcome::Exited(_) => match self.factory.decode_helper_result(name, stdout, stderr, &outcome) {
                Ok(mut value) => {
                    if let Some(object) = value.as_object_mut() {
                        object.insert("effective_timeout_ms".into(), json!(effective_timeout_ms));
                    }
                    json!({"content":[{"type":"text","text":value.to_string()}], "isError":false})
                }
                Err(_) => typed("invalid_result"),
            },
            // §3 step 6: no retry, no persisted result; written files stay in /work.
            HelperOutcome::RunEnded => typed("run_ended"),
            HelperOutcome::TimedOut => typed("helper_timed_out"),
            HelperOutcome::OomKilled => typed("helper_oom_killed"),
            HelperOutcome::Signalled(_) => typed("helper_signalled"),
            HelperOutcome::StartFailed => typed("helper_unavailable"),
        }
    }
}

/// Resolves once the request's live authority fails (revocation, principal
/// generation, run end). Unreadable authority counts as revoked (fail closed).
pub(super) async fn revoked(context: RequestContext) {
    loop {
        tokio::time::sleep(LIVE_POLL).await;
        let live = context.clone();
        if crate::authority::verify_blocking(move || live.verify_live(crate::authority::now_seconds()?)).await.is_err() {
            return;
        }
    }
}

pub(super) fn helper_refusal(error: HelperError) -> Value {
    match error {
        HelperError::Denied => refused(),
        HelperError::Uncertain => uncertain(),
        HelperError::FeatureMissing => {
            let status = error.code();
            json!({"content":[{"type":"text","text":json!({"status":status,
                "reason":ipc::HELPER_FEATURE_MISSING_REASON, "message":message(status)}).to_string()}],
                "isError":true, "helper_status":status})
        }
        _ => typed(error.code()),
    }
}

/// A typed per-call result (§5). Text is fixed; no path, id or content.
pub(super) fn typed(status: &'static str) -> Value {
    json!({"content":[{"type":"text","text":json!({"status":status, "message":message(status)}).to_string()}],
        "isError":true, "helper_status":status})
}

fn message(status: &str) -> &'static str {
    match status {
        "helper_parent_offline" => "此員工的執行環境沒有對外網路，無法錄製瀏覽器。",
        "helper_recording_unsupported" => "這台主機無法在隔離環境內啟動瀏覽器，錄製已停用。",
        "artifact_modified" => "錄製檔案已被修改，與保存的雜湊不符，已拒絕使用。",
        "helper_busy" => "此執行的輔助程式忙碌中，請稍後再試。",
        "run_time_exhausted" => "本次執行剩餘時間不足，這次操作未執行。",
        "helper_cgroup_unavailable" => "主機無法限制輔助程式資源，這次操作未執行。",
        "run_ended" => "執行已結束，結果未保存；已寫入工作區的檔案保持不變。",
        "helper_timed_out" => "輔助程式執行逾時，已終止。",
        "helper_oom_killed" => "輔助程式記憶體不足，已終止。",
        "helper_signalled" => "輔助程式被中斷，無法確認結果。",
        "invalid_result" => "輔助程式結果無法驗證，已捨棄。",
        _ => "隔離執行輔助程式目前無法使用，這次操作未執行。",
    }
}

#[cfg(test)]
#[path = "execution_in_run_tests.rs"]
mod tests;
