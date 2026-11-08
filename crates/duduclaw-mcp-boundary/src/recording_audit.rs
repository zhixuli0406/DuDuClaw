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
