//! HX6 security fix round (finding 6): content-free audit facts of in-run
//! browser recordings. The boundary has no audit writer of its own; the
//! gateway installs a sink (`ExecutionToolBridge::with_recording_audit`) that
//! appends them to `security_audit.jsonl`. Every fact names the employee and
//! the run, and the call once spawnd started one (a start refusal has none);
//! none carries a URL, page content, artifact bytes or the driver result
//! beyond its status/reason codes.
use duduclaw_isolation::{execution::RunHandle, ids::EmployeeId, ipc::HelperCallId};
use serde_json::{Map, Value, json};

/// Which recording transition the gateway observed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordingAuditKind {
    /// The employee's `browser_record_start` got `HelperStarted` and a row.
    Started,
    /// The employee's same-run `browser_record_stop` sent `HelperStop`.
    StopRequested,
    /// Live authority failed (revocation / principal generation) [F16].
    RevokedStop,
    /// The helper exited 0 but its result did not decode (never stored as a recording).
    InvalidResult,
    /// The collector's bound passed; only the slot was cancelled.
    CollectorTimeout,
    /// HX6 round 3: a helper-level start refusal (no call started; detail
    /// `reason` = the returned `helper_status`, `gate` = `gateway` / `spawnd`
    /// for `helper_recording_unsupported`).
    StartRefused,
    /// HX6 round 3: the call ended without a decodable result and was stored
    /// as `uncertain` with a fixed reason (`browser_tree_survived`,
    /// `helper_recording_unsupported` (exit 78), `helper_timed_out`,
    /// `helper_oom_killed`, `helper_failed`, `helper_unavailable`, `run_killed`).
    Failed,
}
impl RecordingAuditKind {
    pub const fn event(self) -> &'static str {
        match self {
            Self::Started => "isolation_recording_started",
            Self::StopRequested => "isolation_recording_stop_requested",
            Self::RevokedStop => "isolation_recording_revoked_stop",
            Self::InvalidResult => "isolation_recording_invalid_result",
            Self::CollectorTimeout => "isolation_recording_collector_timeout",
            Self::StartRefused => "isolation_recording_start_refused",
            Self::Failed => "isolation_recording_failed",
        }
    }
    /// Failures are warnings; ordinary transitions are info.
    pub const fn warning(self) -> bool {
        matches!(self, Self::InvalidResult | Self::CollectorTimeout | Self::RevokedStop | Self::StartRefused | Self::Failed)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RecordingAuditEvent {
    pub kind: RecordingAuditKind,
    pub employee: EmployeeId,
    pub run: RunHandle,
    /// `None` only for [`RecordingAuditKind::StartRefused`] (nothing was started).
    pub call: Option<HelperCallId>,
    /// Small fixed-vocabulary extras (`stop_by`, `reason`, `outcome`, timeouts).
    pub detail: Map<String, Value>,
}
fn hex(bytes: &[u8]) -> String { bytes.iter().map(|byte| format!("{byte:02x}")).collect() }
impl RecordingAuditEvent {
    pub fn new(kind: RecordingAuditKind, employee: EmployeeId, run: RunHandle, call: HelperCallId) -> Self {
        Self { kind, employee, run, call: Some(call), detail: Map::new() }
    }
    /// A start refused before any call existed; `reason` is the fixed status code returned.
    pub fn refused(employee: EmployeeId, run: RunHandle, reason: &'static str) -> Self {
        Self { kind: RecordingAuditKind::StartRefused, employee, run, call: None, detail: Map::new() }.with("reason", json!(reason))
    }
    pub fn with(mut self, key: &str, value: Value) -> Self { self.detail.insert(key.to_owned(), value); self }
    /// The audit record's fields: identities first, then the extras.
    pub fn fields(&self) -> Value {
        let mut fields = Map::new();
        fields.insert("employee".into(), json!(hex(&self.employee.0)));
        fields.insert("run".into(), json!(hex(&self.run.0 .0)));
        fields.insert("call".into(), self.call.map_or(Value::Null, |call| json!(call.hex())));
        for (key, value) in &self.detail {
            if !fields.contains_key(key) { fields.insert(key.clone(), value.clone()); }
        }
        Value::Object(fields)
    }
}

/// N3: window of [`StartRefusedLimiter`]: one record per key, then at most one
/// counter summary per window.
pub const START_REFUSED_WINDOW: std::time::Duration = std::time::Duration::from_secs(60);
/// Bound on distinct `(employee, run, reason)` keys held at once.
const START_REFUSED_KEYS: usize = 1024;

struct RefusedState { window_start: std::time::Instant, suppressed: u64, first: RecordingAuditEvent }
/// N3: merges `StartRefused` audit records per `(employee, run, reason)` so a
/// looping caller cannot flood the audit log (like spawnd's audit budget): the
/// first refusal is recorded at once; further ones in the window only count,
/// and the count is written as one summary (`suppressed`, `window_secs`) when
/// the next refusal of that key arrives after the window, or when any later
/// refusal sweeps the expired key. Memory is bounded; at the bound a new key's
/// record is dropped (the log already holds the flood's older records).
/// Other event kinds pass through untouched.
#[derive(Default)]
pub struct StartRefusedLimiter { keys: std::sync::Mutex<std::collections::HashMap<([u8; 16], [u8; 16], String), RefusedState>> }
impl StartRefusedLimiter {
    /// The records to write now for `event` (possibly none).
    pub fn admit(&self, event: RecordingAuditEvent, now: std::time::Instant) -> Vec<RecordingAuditEvent> {
        if event.kind != RecordingAuditKind::StartRefused { return vec![event]; }
        let reason = event.detail.get("reason").and_then(Value::as_str).unwrap_or_default().chars().take(64).collect::<String>();
        let key = (event.employee.0, event.run.0 .0, reason);
        let mut keys = self.keys.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let expired = |state: &RefusedState| now.saturating_duration_since(state.window_start) >= START_REFUSED_WINDOW;
        let mut out = Vec::new();
        if keys.len() >= START_REFUSED_KEYS {
            // Sweep expired keys, writing their pending summaries.
            keys.retain(|other, state| {
                if !expired(state) || *other == key { return true; }
                if state.suppressed > 0 { out.push(Self::summary(state)); }
                false
            });
        }
        let full = keys.len() >= START_REFUSED_KEYS;
        match keys.get_mut(&key) {
            Some(state) if !expired(state) => state.suppressed = state.suppressed.saturating_add(1),
            Some(state) => {
                // This refusal opens the next window: it is written as the
                // first record when nothing was suppressed, else it is counted
                // into the next summary after this window's summary.
                if state.suppressed > 0 {
                    out.push(Self::summary(state));
                    *state = RefusedState { window_start: now, suppressed: 1, first: event };
                } else {
                    out.push(event.clone());
                    *state = RefusedState { window_start: now, suppressed: 0, first: event };
                }
            }
            None if full => {}
            None => {
                out.push(event.clone());
                keys.insert(key, RefusedState { window_start: now, suppressed: 0, first: event });
            }
        }
        out
    }
    fn summary(state: &RefusedState) -> RecordingAuditEvent {
        state.first.clone().with("suppressed", json!(state.suppressed)).with("window_secs", json!(START_REFUSED_WINDOW.as_secs()))
    }
}

/// Installed by the gateway; must not block for long (it is called from the
/// boundary's tasks) and must never fail the recording.
pub trait RecordingAudit: Send + Sync {
    fn record(&self, event: RecordingAuditEvent);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hx6fix_recording_audit_facts_carry_ids_and_codes_only() {
        let call = HelperCallId::new([0xab; 16]).unwrap();
        let event = RecordingAuditEvent::new(RecordingAuditKind::StopRequested, EmployeeId([1; 16]),
            RunHandle(duduclaw_isolation::ids::RunId([2; 16])), call)
            .with("stop_by", json!("stop")).with("employee", json!("forged"));
        let fields = event.fields();
        assert_eq!(fields["employee"], "01".repeat(16), "identities cannot be overwritten by extras");
        assert_eq!(fields["run"], "02".repeat(16));
        assert_eq!(fields["call"], "ab".repeat(16));
        assert_eq!(fields["stop_by"], "stop");
        let names = [RecordingAuditKind::Started, RecordingAuditKind::StopRequested, RecordingAuditKind::RevokedStop,
            RecordingAuditKind::InvalidResult, RecordingAuditKind::CollectorTimeout].map(RecordingAuditKind::event);
        assert_eq!(names, ["isolation_recording_started", "isolation_recording_stop_requested", "isolation_recording_revoked_stop",
            "isolation_recording_invalid_result", "isolation_recording_collector_timeout"]);
    }
    /// N3: refusals merge per (employee, run, reason): first record, then one
    /// summary per window; other keys and kinds are independent.
    #[test]
    fn hx6c_start_refused_audit_is_rate_limited_per_employee_run_reason() {
        use std::time::{Duration, Instant};
        let run = |n: u8| RunHandle(duduclaw_isolation::ids::RunId([n; 16]));
        let refused = |employee: u8, run_n: u8, reason: &'static str| RecordingAuditEvent::refused(EmployeeId([employee; 16]), run(run_n), reason);
        let limiter = StartRefusedLimiter::default();
        let t0 = Instant::now();
        let first = limiter.admit(refused(1, 2, "helper_recording_unsupported").with("gate", json!("spawnd")), t0);
        assert_eq!(first.len(), 1);
        assert!(first[0].fields().get("suppressed").is_none());
        for step in 1..=50u64 {
            assert!(limiter.admit(refused(1, 2, "helper_recording_unsupported"), t0 + Duration::from_millis(step)).is_empty());
        }
        // A different reason, run or employee is its own key.
        for other in [refused(1, 2, "helper_unavailable"), refused(1, 3, "helper_recording_unsupported"), refused(9, 2, "helper_recording_unsupported")] {
            assert_eq!(limiter.admit(other, t0 + Duration::from_secs(1)).len(), 1);
        }
        // Other kinds are never limited.
        let call = HelperCallId::new([1; 16]).unwrap();
        for _ in 0..3 {
            assert_eq!(limiter.admit(RecordingAuditEvent::new(RecordingAuditKind::Failed, EmployeeId([1; 16]), run(2), call), t0).len(), 1);
        }
        // After the window: one summary with the count (and the original gate); the new refusal is counted.
        let later = t0 + START_REFUSED_WINDOW;
        let summary = limiter.admit(refused(1, 2, "helper_recording_unsupported"), later);
        assert_eq!(summary.len(), 1);
        let fields = summary[0].fields();
        assert_eq!((fields["suppressed"].as_u64(), fields["gate"].as_str(), fields["call"].clone()), (Some(50), Some("spawnd"), Value::Null));
        assert_eq!(fields["reason"], "helper_recording_unsupported");
        assert!(limiter.admit(refused(1, 2, "helper_recording_unsupported"), later + Duration::from_secs(1)).is_empty());
        let next = limiter.admit(refused(1, 2, "helper_recording_unsupported"), later + START_REFUSED_WINDOW);
        assert_eq!(next[0].fields()["suppressed"].as_u64(), Some(2));
        // A quiet key's next refusal after a window with nothing suppressed is a plain record.
        let plain = limiter.admit(refused(9, 2, "helper_recording_unsupported"), t0 + START_REFUSED_WINDOW * 3);
        assert!(plain.len() == 1 && plain[0].fields().get("suppressed").is_none());
    }
    /// N3: the key table is bounded; a flood of distinct keys cannot grow it.
    #[test]
    fn hx6c_start_refused_limiter_is_bounded() {
        let limiter = StartRefusedLimiter::default();
        let now = std::time::Instant::now();
        let mut written = 0;
        for n in 0..(START_REFUSED_KEYS + 50) {
            let run = RunHandle(duduclaw_isolation::ids::RunId([(n % 251) as u8, (n / 251) as u8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]));
            written += limiter.admit(RecordingAuditEvent::refused(EmployeeId([1; 16]), run, "x"), now).len();
        }
        assert_eq!(written, START_REFUSED_KEYS);
        assert_eq!(limiter.keys.lock().unwrap().len(), START_REFUSED_KEYS);
    }
    /// HX6 round 3: start refusals carry no call (and a forged `call` extra
    /// cannot invent one); refusals and failures are warnings.
    #[test]
    fn hx6r3_recording_audit_refusal_and_failure_facts() {
        let run = RunHandle(duduclaw_isolation::ids::RunId([2; 16]));
        let refused = RecordingAuditEvent::refused(EmployeeId([1; 16]), run, "helper_recording_unsupported")
            .with("gate", json!("spawnd")).with("call", json!("forged"));
        let fields = refused.fields();
        assert_eq!(fields["call"], Value::Null, "no call was started");
        assert_eq!((fields["reason"].as_str(), fields["gate"].as_str()), (Some("helper_recording_unsupported"), Some("spawnd")));
        assert_eq!(refused.kind.event(), "isolation_recording_start_refused");
        assert_eq!(RecordingAuditKind::Failed.event(), "isolation_recording_failed");
        assert!(RecordingAuditKind::StartRefused.warning() && RecordingAuditKind::Failed.warning());
        assert!(!RecordingAuditKind::Started.warning());
    }
}
