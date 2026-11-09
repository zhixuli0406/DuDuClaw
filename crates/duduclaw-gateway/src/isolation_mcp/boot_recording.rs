//! HX6 (D8 revision §4) Admin control of in-run recordings and, security fix
//! round finding 6, their audit. Split out of `boot.rs` (file size).
//!
//! Every Admin status read and every Admin stop is audited, success AND
//! failure, with the actor, the recording's employee, run and call, who asked
//! it to stop (`stop_by`) and the outcome/reason code. The boundary's own
//! recording facts (employee start/stop, revocation-triggered stop, invalid
//! result, collector timeout and, HX6 round 3, helper-level start refusals
//! incl. spawnd's `helper_recording_unsupported` gate (`call: null`) and
//! terminal outcomes stored only as an `uncertain` reason: browser tree
//! survived, exit 78, timeout, OOM) reach `security_audit.jsonl` through
//! [`GatewayRecordingAudit`]. No URL, page content or artifact is logged.
use super::*;
use duduclaw_mcp_boundary::{recording_audit::{RecordingAudit, RecordingAuditEvent, StartRefusedLimiter},
    recording_jobs::{RecordingJobs, RecordingKey}};
use duduclaw_security::audit::{append_audit_event, AuditEvent, Severity};

fn hex(bytes: &[u8]) -> String { bytes.iter().map(|byte| format!("{byte:02x}")).collect() }
fn reason(error: &VerificationError) -> &'static str {
    match error {
        VerificationError::Invalid => "invalid",
        VerificationError::Revoked => "revoked",
        VerificationError::BindingMismatch => "binding_mismatch",
        VerificationError::InvalidTime => "invalid_time",
        _ => "unavailable",
    }
}

/// What the Admin call learned about the recording (for the audit record).
#[derive(Default)]
struct Facts { key: Option<RecordingKey>, employee: Option<duduclaw_isolation::ids::EmployeeId>, stop_by: Option<&'static str> }

/// Dashboard control of an in-run recording (HX6, D8 revision §4). Live run:
/// `HelperStop` for exactly that `(run, call)` through spawnd; ended: the
/// stored result. It never issues `HelperCall`, never mints an employee
/// context and never stops the run. Called on a blocking thread.
pub(crate) fn control_recording(home: &Path, ctx: &duduclaw_auth::UserContext, id: &str, stop: bool)
    -> Result<serde_json::Value, VerificationError> {
    let mut facts = Facts::default();
    let result = control(home, ctx, id, stop, &mut facts);
    let (severity, outcome) = match &result { Ok(_) => (Severity::Info, "ok"), Err(_) => (Severity::Warning, "failed") };
    let event = if stop { "isolation_recording_admin_stop" } else { "isolation_recording_admin_status" };
    let status = result.as_ref().ok().and_then(|value| value["status"].as_str()).map(str::to_owned);
    let stored_reason = result.as_ref().ok().and_then(|value| value["result"]["reason"].as_str()).map(str::to_owned);
    append_audit_event(home, &AuditEvent::new(event, "gateway", severity, serde_json::json!({
        "actor": ctx.user_id,
        "employee": facts.employee.map(|employee| hex(&employee.0)),
        "run": facts.key.map(|key| hex(&key.run.0 .0)),
        "call": facts.key.map(|key| key.id()),
        "job": id.chars().take(64).collect::<String>(),
        "stop_by": facts.stop_by,
        "outcome": outcome,
        "status": status,
        "reason": match &result { Ok(_) => stored_reason, Err(error) => Some(reason(error).to_owned()) },
    })));
    result
}

fn control(home: &Path, ctx: &duduclaw_auth::UserContext, id: &str, stop: bool, facts: &mut Facts)
    -> Result<serde_json::Value, VerificationError> {
    crate::approval::require_current_dashboard_role_in_home(home, ctx, duduclaw_auth::UserRole::Admin)
        .map_err(|_| VerificationError::Invalid)?;
    let call = duduclaw_mcp_boundary::recording_jobs::parse_id(id)?;
    let owner = OWNER.get().filter(|owner| owner.home == home).ok_or(VerificationError::Unavailable)?;
    // Trusted host read, for the audit's employee/run only (control re-reads).
    if let Ok(row) = owner.recording_jobs.admin_read(call) { facts.key = Some(row.key); facts.employee = Some(row.employee); }
    let runtime = tokio::runtime::Handle::try_current().map_err(|_| VerificationError::Unavailable)?;
    let result = runtime.block_on(duduclaw_mcp_boundary::execution::control_recording(
        owner.recording_jobs.clone(), owner.backend.clone(), id, stop));
    if let Some(key) = facts.key {
        facts.stop_by = owner.recording_jobs.stop_by(key).ok().flatten().map(|by| by.as_str());
    }
    result
}

/// The boundary's recording facts, appended to `security_audit.jsonl` off
/// the async worker threads.
pub(super) struct GatewayRecordingAudit { home: PathBuf, refused: StartRefusedLimiter }
impl GatewayRecordingAudit {
    pub(super) fn new(home: PathBuf) -> Self { Self { home, refused: StartRefusedLimiter::default() } }
}
impl RecordingAudit for GatewayRecordingAudit {
    fn record(&self, event: RecordingAuditEvent) {
        // N3: start refusals are merged per (employee, run, reason).
        for event in self.refused.admit(event, std::time::Instant::now()) {
            let severity = if event.kind.warning() { Severity::Warning } else { Severity::Info };
            let record = AuditEvent::new(event.kind.event(), "gateway", severity, event.fields());
            let home = self.home.clone();
            match tokio::runtime::Handle::try_current() {
                Ok(runtime) => { runtime.spawn_blocking(move || append_audit_event(&home, &record)); }
                Err(_) => append_audit_event(&home, &record),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Finding 6: Admin status reads and stops are audited on failure too,
    /// with actor, job and a reason code (no recording state is touched).
    #[test]
    fn hx6fix_admin_recording_control_audits_both_outcomes() {
        let home = tempfile::tempdir().unwrap();
        let ctx = duduclaw_auth::UserContext { credential_epoch: 0, user_id: "u-admin".into(), email: "a@example.com".into(),
            role: duduclaw_auth::UserRole::Admin, agent_access: Default::default(), must_change_password: false,
            team_access: Default::default(), acl_generation: 0 };
        let id = "ab".repeat(16);
        for stop in [false, true] { assert!(control_recording(home.path(), &ctx, &id, stop).is_err(), "no users.db: refused"); }
        let log = std::fs::read_to_string(home.path().join("security_audit.jsonl")).unwrap();
        let events: Vec<serde_json::Value> = log.lines().map(|line| serde_json::from_str(line).unwrap()).collect();
        let kinds: Vec<_> = events.iter().map(|event| event["event_type"].as_str().unwrap().to_owned()).collect();
        assert_eq!(kinds, ["isolation_recording_admin_status", "isolation_recording_admin_stop"]);
        for event in &events {
            let details = &event["details"];
            assert_eq!((details["actor"].as_str(), details["job"].as_str()), (Some("u-admin"), Some(id.as_str())));
            assert_eq!(details["outcome"].as_str(), Some("failed"));
            // Role refused (no users.db), or no booted owner: either way a named code.
            assert!(matches!(details["reason"].as_str(), Some("invalid" | "unavailable")), "{details}");
            for key in ["employee", "run", "call", "stop_by", "status"] { assert!(details.get(key).is_some(), "{key}"); }
        }
        assert_eq!(reason(&VerificationError::Unavailable), "unavailable");
        assert_eq!(reason(&VerificationError::Revoked), "revoked");
    }
    /// N3: a flood of identical start refusals reaches `security_audit.jsonl`
    /// as one record; a different reason is its own record.
    #[test]
    fn hx6c_gateway_sink_merges_start_refused_records() {
        use duduclaw_isolation::{execution::RunHandle, ids::{EmployeeId, RunId}};
        let home = tempfile::tempdir().unwrap();
        let sink = GatewayRecordingAudit::new(home.path().to_path_buf());
        let refused = |reason: &'static str| RecordingAuditEvent::refused(EmployeeId([1; 16]), RunHandle(RunId([2; 16])), reason);
        for _ in 0..20 { sink.record(refused("helper_recording_unsupported")); }
        sink.record(refused("helper_unavailable"));
        let log = std::fs::read_to_string(home.path().join("security_audit.jsonl")).unwrap();
        let events: Vec<serde_json::Value> = log.lines().map(|line| serde_json::from_str(line).unwrap()).collect();
        assert_eq!(events.len(), 2, "{log}");
        assert!(events.iter().all(|event| event["event_type"] == "isolation_recording_start_refused"));
    }
}
