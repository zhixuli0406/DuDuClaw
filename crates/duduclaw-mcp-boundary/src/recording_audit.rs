//! HX6 security fix round (finding 6): content-free audit facts of in-run
//! browser recordings. The boundary has no audit writer of its own; the
//! gateway installs a sink (`ExecutionToolBridge::with_recording_audit`) that
//! appends them to `security_audit.jsonl`. Every fact names the employee, the
//! run and the call; none carries a URL, page content, artifact bytes or the
//! driver result beyond its status/reason codes.
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
}
impl RecordingAuditKind {
    pub const fn event(self) -> &'static str {
        match self {
            Self::Started => "isolation_recording_started",
            Self::StopRequested => "isolation_recording_stop_requested",
            Self::RevokedStop => "isolation_recording_revoked_stop",
            Self::InvalidResult => "isolation_recording_invalid_result",
            Self::CollectorTimeout => "isolation_recording_collector_timeout",
        }
    }
    /// Failures are warnings; ordinary transitions are info.
    pub const fn warning(self) -> bool { matches!(self, Self::InvalidResult | Self::CollectorTimeout | Self::RevokedStop) }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RecordingAuditEvent {
    pub kind: RecordingAuditKind,
    pub employee: EmployeeId,
    pub run: RunHandle,
    pub call: HelperCallId,
    /// Small fixed-vocabulary extras (`stop_by`, `reason`, `outcome`, timeouts).
    pub detail: Map<String, Value>,
}
fn hex(bytes: &[u8]) -> String { bytes.iter().map(|byte| format!("{byte:02x}")).collect() }
impl RecordingAuditEvent {
    pub fn new(kind: RecordingAuditKind, employee: EmployeeId, run: RunHandle, call: HelperCallId) -> Self {
        Self { kind, employee, run, call, detail: Map::new() }
    }
    pub fn with(mut self, key: &str, value: Value) -> Self { self.detail.insert(key.to_owned(), value); self }
    /// The audit record's fields: identities first, then the extras.
    pub fn fields(&self) -> Value {
        let mut fields = Map::new();
        fields.insert("employee".into(), json!(hex(&self.employee.0)));
        fields.insert("run".into(), json!(hex(&self.run.0 .0)));
        fields.insert("call".into(), json!(self.call.hex()));
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
}
