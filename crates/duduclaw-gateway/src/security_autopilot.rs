//! OS security line P0 — C1 "producer 甲": bridge gateway-side security audit
//! events onto the autopilot bus as `AutopilotEvent::SecurityEvent`.
//!
//! ## Why this exists
//!
//! `duduclaw-security`'s 24 protection modules (input guard, contract
//! verifier, circuit breaker, …) already write to `security_audit.jsonl` via
//! `duduclaw_security::audit::append_audit_event` / `log_injection_detected`
//! / `log_circuit_breaker_trip` / `log_contract_violation`. Until this module
//! that log was a write-only sink: nothing downstream of it ever ran. The
//! `AutopilotEngine` (event → conditions → notify/delegate/run_skill, with a
//! circuit breaker of its own) is a fully-built reaction pipeline that simply
//! never received a security-shaped event. This module is the missing wire.
//!
//! ## Direction discipline
//!
//! `duduclaw-security` MUST NOT depend on `duduclaw-gateway` (the same
//! constraint `security_posture.rs`'s `EscalationFloor` doc comment already
//! calls out for `ErrorCategory`). So the emission logic lives here, in the
//! gateway crate, called FROM gateway call sites that already invoke the
//! audit functions — never the other way around.
//!
//! ## Severity mapping (a documented deviation from the original design ask)
//!
//! The design brief described a 4-level `severity` (`info`/`warning`/`error`/
//! `critical`). `duduclaw_security::audit::Severity` — the actual, shipping
//! enum every audit call site already uses — has exactly THREE variants
//! (`Info`/`Warning`/`Critical`). Inventing a 4th "error" bucket would mean
//! either a second, parallel severity type (a second source of truth for the
//! same concept) or widening `audit::Severity` itself, which ripples through
//! every existing `match` on it (`count_events_since`, `posture_from_counts`
//! callers, …) for a distinction no call site currently makes. This module
//! carries the real 3-level severity verbatim: `"info"` / `"warning"` /
//! `"critical"`. Autopilot rules that want "error-or-worse" should match
//! `{"field":"severity","op":"in","value":["warning","critical"]}`.
//!
//! ## Producer coverage
//!
//! Two producers feed this module:
//!   1. [`audit_and_emit`] — a drop-in replacement for
//!      `duduclaw_security::audit::append_audit_event` used at every gateway
//!      call site that constructs its own `AuditEvent` (the majority —
//!      `handlers.rs`, `server.rs`, `wiki_ingest.rs`, `license_serve.rs`, …).
//!      Same signature, so swapping the call is mechanical; it appends to the
//!      audit log exactly as before AND mirrors the event onto the autopilot
//!      bus when severity ≥ Warning.
//!   2. [`emit_injection_detected`] / [`emit_circuit_breaker_trip`] /
//!      [`emit_contract_violation`] / [`emit_safety_word_triggered`] /
//!      [`emit_tool_hallucination`] / [`emit_config_changed`] /
//!      [`emit_os_update_applied`] / [`emit_git_credentials_granted`] — thin
//!      companions for the audit helper functions that DON'T take a
//!      pre-built `AuditEvent` (each hardcodes its own severity internally,
//!      so the audit crate never exposes the event it built), called
//!      alongside them at their gateway call sites
//!      (`autopilot_engine.rs`'s own perception sanitizer, `wiki_ingest.rs`,
//!      `channel_reply.rs`, `cron_scheduler.rs`, `dispatcher.rs`,
//!      `handlers.rs`, `claude_runner.rs`).
//!   3. [`crate::posture_watch`] is the third producer (SecurityPosture
//!      transitions) — a separate module since it drives its own poll loop
//!      rather than piggybacking on an existing call site.
//!
//! Coverage is NOT exhaustive — see the P0 handoff report (not duplicated
//! here to avoid two copies drifting apart) for the exact list of gateway
//! call sites left unwired: they are either genuinely Info-only
//! (`log_os_boot`, `log_os_update_blessed` — filtered out below even if
//! wired) or a single Critical-severity site
//! (`update_report_reconcile.rs`'s `log_os_rollback_detected`) deliberately
//! left untouched because that file was under active concurrent edit by
//! another work stream during this change.

use std::sync::OnceLock;

use duduclaw_security::audit::{AuditEvent, Severity};
use tokio::sync::broadcast;

use crate::autopilot_engine::AutopilotEvent;

/// Source tag distinguishing the two C1 producers on the `SecurityEvent`'s
/// `source` field (design §2 支柱三 C1): `"audit"` for every audit-log-backed
/// emission in this module, `"posture"` for `posture_watch`'s
/// SecurityPosture-transition emissions.
pub const SOURCE_AUDIT: &str = "audit";

/// Process-global handle to the autopilot broadcast bus. Set once at gateway
/// boot (`server.rs`, alongside `handler.set_autopilot_event_tx`) — mirrors
/// the existing `CHANNEL_STATUS_PATH` / `channel_reply::agent_config_events`
/// `OnceLock` convention in this crate, chosen because many call sites this
/// module wires (e.g. `wiki_ingest.rs`, `power_local.rs`) have no `&self` on
/// `MethodHandler` or `ReplyContext` to thread a sender through.
static SECURITY_EVENT_TX: OnceLock<broadcast::Sender<AutopilotEvent>> = OnceLock::new();

/// Wire the autopilot bus sender. Idempotent (first call wins) — safe to call
/// even if some future test harness calls it more than once.
pub fn set_security_event_tx(tx: broadcast::Sender<AutopilotEvent>) {
    let _ = SECURITY_EVENT_TX.set(tx);
}

fn severity_label(s: &Severity) -> &'static str {
    match s {
        Severity::Info => "info",
        Severity::Warning => "warning",
        Severity::Critical => "critical",
    }
}

/// Pure event construction, split out from [`emit_security_autopilot_event`]
/// so the severity-filter/field-mapping logic is unit-testable without
/// touching the process-global `SECURITY_EVENT_TX` (see this module's test
/// notes — only ONE test in this file is allowed to touch that global,
/// mirroring `dashboard_navigate.rs`'s `OnceLock` testing convention).
/// `None` for `Severity::Info` — the filtered-out case.
fn build_event(
    severity: &Severity,
    event_type: &str,
    agent_id: Option<&str>,
    source: &str,
) -> Option<AutopilotEvent> {
    if matches!(severity, Severity::Info) {
        return None;
    }
    Some(AutopilotEvent::SecurityEvent {
        severity: severity_label(severity).to_string(),
        event_type: event_type.to_string(),
        agent_id: agent_id.unwrap_or("system").to_string(),
        source: source.to_string(),
    })
}

/// The thin producer function itself. Builds and best-effort-sends an
/// `AutopilotEvent::SecurityEvent`.
///
/// Fails open, silently, in both documented ways:
///   - `severity < Warning` (i.e. `Info`) — never reaches the bus. The audit
///     log remains the source of truth for informational events; flooding
///     the autopilot bus (and every rule's condition evaluation) with
///     routine Info traffic would defeat the point of a severity filter.
///   - no bus wired yet (autopilot disabled — `[dispatch]`/task-board/
///     autopilot-store missing — or the narrow pre-boot window before
///     `server.rs` calls `set_security_event_tx`) — `send` is a no-op; this
///     matches every other autopilot producer in this crate (`os_events`,
///     `tick_source`, …), none of which treat a missing subscriber as an
///     error.
pub fn emit_security_autopilot_event(
    severity: &Severity,
    event_type: &str,
    agent_id: Option<&str>,
    source: &str,
) {
    let Some(event) = build_event(severity, event_type, agent_id, source) else {
        return;
    };
    let Some(tx) = SECURITY_EVENT_TX.get() else {
        return;
    };
    // Best-effort: a `SendError` here means zero receivers (no
    // `AutopilotEngine` running) — never a reason to fail the caller's own
    // audit-log write, which has already completed by the time this runs.
    let _ = tx.send(event);
}

/// Drop-in replacement for `duduclaw_security::audit::append_audit_event`:
/// identical signature and behavior, PLUS mirrors the event onto the
/// autopilot bus (severity ≥ Warning only, per
/// [`emit_security_autopilot_event`]). Prefer this over calling
/// `duduclaw_security::audit::append_audit_event` directly at any NEW
/// gateway call site so it is automatically wired into C1 without a second
/// manual step.
pub fn audit_and_emit(home_dir: &std::path::Path, event: &AuditEvent) {
    duduclaw_security::audit::append_audit_event(home_dir, event);
    emit_security_autopilot_event(
        &event.severity,
        &event.event_type,
        Some(event.agent_id.as_str()),
        SOURCE_AUDIT,
    );
}

/// Severity `log_injection_detected` itself computes (`blocked` ⇒ Critical,
/// else Warning) — duplicated here (not imported; the audit crate doesn't
/// expose it standalone) so [`emit_injection_detected`]'s bus severity is
/// guaranteed to match what actually landed in the audit log. Pure, so the
/// mapping is unit-testable without touching the global bus.
fn injection_detected_severity(blocked: bool) -> Severity {
    if blocked {
        Severity::Critical
    } else {
        Severity::Warning
    }
}

/// Companion to `duduclaw_security::audit::log_injection_detected`, which
/// does not expose the `AuditEvent` it builds internally. Call directly
/// alongside `log_injection_detected` at its gateway call sites (never
/// inside `duduclaw-security` itself).
pub fn emit_injection_detected(agent_id: &str, blocked: bool) {
    let severity = injection_detected_severity(blocked);
    emit_security_autopilot_event(&severity, "prompt_injection", Some(agent_id), SOURCE_AUDIT);
}

/// Companion to `duduclaw_security::audit::log_circuit_breaker_trip` (always
/// Warning severity, matching that function's own hardcoded severity).
pub fn emit_circuit_breaker_trip(agent_id: &str) {
    emit_security_autopilot_event(
        &Severity::Warning,
        "circuit_breaker_tripped",
        Some(agent_id),
        SOURCE_AUDIT,
    );
}

/// Companion to `duduclaw_security::audit::log_contract_violation` (always
/// Critical severity, matching that function's own hardcoded severity).
pub fn emit_contract_violation(agent_id: &str) {
    emit_security_autopilot_event(
        &Severity::Critical,
        "contract_violation",
        Some(agent_id),
        SOURCE_AUDIT,
    );
}

/// Companion to `duduclaw_security::audit::log_safety_word` (always Critical
/// severity, matching that function's own hardcoded severity — a `!STOP` /
/// `!STOP ALL` safety word is a Critical event regardless of which of its
/// three call sites in `channel_reply.rs` fired it).
pub fn emit_safety_word_triggered(agent_id: &str) {
    emit_security_autopilot_event(
        &Severity::Critical,
        "safety_word_triggered",
        Some(agent_id),
        SOURCE_AUDIT,
    );
}

/// Companion to `duduclaw_security::audit::log_tool_hallucination` (always
/// Critical severity, matching that function's own hardcoded severity).
pub fn emit_tool_hallucination(agent_id: &str) {
    emit_security_autopilot_event(
        &Severity::Critical,
        "tool_hallucination",
        Some(agent_id),
        SOURCE_AUDIT,
    );
}

/// Companion to `duduclaw_security::audit::log_config_changed` (always
/// Warning severity; that function itself hardcodes `agent_id` to
/// `"dashboard"` — mirrored here rather than accepting a caller-supplied
/// value so the two can never drift apart).
pub fn emit_config_changed() {
    emit_security_autopilot_event(
        &Severity::Warning,
        "config_changed",
        Some("dashboard"),
        SOURCE_AUDIT,
    );
}

/// Companion to `duduclaw_security::audit::log_os_update_applied` (always
/// Warning severity, matching that function's own hardcoded severity).
/// `actor` mirrors that function's own first non-`home_dir` parameter.
pub fn emit_os_update_applied(actor: &str) {
    emit_security_autopilot_event(
        &Severity::Warning,
        "os_update_applied",
        Some(actor),
        SOURCE_AUDIT,
    );
}

/// Companion to `duduclaw_security::audit::log_git_credentials_granted`
/// (always Warning severity, matching that function's own hardcoded
/// severity).
pub fn emit_git_credentials_granted(agent_id: &str) {
    emit_security_autopilot_event(
        &Severity::Warning,
        "git_credentials_env_granted",
        Some(agent_id),
        SOURCE_AUDIT,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Pure logic — no global state, safe under parallel test execution ──

    #[test]
    fn info_severity_builds_nothing() {
        assert!(build_event(&Severity::Info, "noise", Some("agent-1"), SOURCE_AUDIT).is_none());
    }

    #[test]
    fn warning_and_critical_build_with_correct_fields() {
        let ev = build_event(
            &Severity::Warning,
            "circuit_breaker_tripped",
            Some("agent-x"),
            SOURCE_AUDIT,
        )
        .expect("warning must build");
        match ev {
            AutopilotEvent::SecurityEvent {
                severity,
                event_type,
                agent_id,
                source,
            } => {
                assert_eq!(severity, "warning");
                assert_eq!(event_type, "circuit_breaker_tripped");
                assert_eq!(agent_id, "agent-x");
                assert_eq!(source, "audit");
            }
            other => panic!("expected SecurityEvent, got {other:?}"),
        }

        let ev = build_event(&Severity::Critical, "contract_violation", None, "posture")
            .expect("critical must build");
        match ev {
            AutopilotEvent::SecurityEvent {
                severity,
                agent_id,
                source,
                ..
            } => {
                assert_eq!(severity, "critical");
                assert_eq!(agent_id, "system", "missing agent_id defaults to system");
                assert_eq!(source, "posture");
            }
            other => panic!("expected SecurityEvent, got {other:?}"),
        }
    }

    #[test]
    fn companion_functions_use_the_documented_severities() {
        assert!(matches!(
            injection_detected_severity(true),
            Severity::Critical
        ));
        assert!(matches!(
            injection_detected_severity(false),
            Severity::Warning
        ));
    }

    #[test]
    fn audit_and_emit_always_writes_the_log_regardless_of_severity() {
        // Exercises the log-write half of `audit_and_emit` without touching
        // the global bus (no `set_security_event_tx` call in this test) —
        // an Info event must still land in `security_audit.jsonl` even
        // though it will never mirror to the autopilot bus.
        let home = tempfile::tempdir().unwrap();
        let event = AuditEvent::new(
            "routine_check",
            "agent-z",
            Severity::Info,
            serde_json::json!({}),
        );
        audit_and_emit(home.path(), &event);
        let log = std::fs::read_to_string(home.path().join("security_audit.jsonl")).unwrap();
        assert!(
            log.contains("routine_check"),
            "log write must happen regardless of severity"
        );
        assert!(log.contains("agent-z"));
    }

    /// The only test in this file allowed to touch [`SECURITY_EVENT_TX`] —
    /// it is a process-global `OnceLock` (first-call-wins), so a second test
    /// calling `set_security_event_tx` with a different sender would
    /// silently make this one flaky under parallel test execution (same
    /// convention as `dashboard_navigate.rs`'s `push_dashboard_navigate_wiring`
    /// test — keep every assertion about the LIVE wiring here, in one place).
    #[test]
    fn end_to_end_wiring_through_the_global_bus() {
        let (tx, mut rx) = broadcast::channel(64);
        set_security_event_tx(tx.clone());
        // A second call must be a no-op (first call wins) — otherwise any
        // other test in this binary that happened to run first would
        // silently steal this test's channel.
        set_security_event_tx(tx);

        // `emit_security_autopilot_event` — Info filtered, Warning delivered.
        emit_security_autopilot_event(&Severity::Info, "noise", Some("a1"), SOURCE_AUDIT);
        assert!(rx.try_recv().is_err(), "Info must never reach the bus");
        emit_security_autopilot_event(&Severity::Warning, "x", Some("a1"), SOURCE_AUDIT);
        assert!(rx.try_recv().is_ok());

        // `audit_and_emit` — Critical AuditEvent mirrors; log write also verified.
        let home = tempfile::tempdir().unwrap();
        let event = AuditEvent::new(
            "prompt_injection",
            "agent-y",
            Severity::Critical,
            serde_json::json!({ "blocked": true }),
        );
        audit_and_emit(home.path(), &event);
        let log = std::fs::read_to_string(home.path().join("security_audit.jsonl")).unwrap();
        assert!(log.contains("prompt_injection") && log.contains("agent-y"));
        let ev = rx
            .try_recv()
            .expect("critical AuditEvent must mirror to the bus");
        assert_eq!(field_severity(ev), "critical");

        // Companion functions round-trip through the same global bus.
        emit_injection_detected("a1", true);
        assert_eq!(field_severity(rx.try_recv().unwrap()), "critical");
        emit_injection_detected("a1", false);
        assert_eq!(field_severity(rx.try_recv().unwrap()), "warning");
        emit_circuit_breaker_trip("a1");
        assert_eq!(field_severity(rx.try_recv().unwrap()), "warning");
        emit_contract_violation("a1");
        assert_eq!(field_severity(rx.try_recv().unwrap()), "critical");
        emit_safety_word_triggered("a1");
        assert_eq!(field_severity(rx.try_recv().unwrap()), "critical");
        emit_tool_hallucination("a1");
        assert_eq!(field_severity(rx.try_recv().unwrap()), "critical");
        emit_config_changed();
        let ev = rx.try_recv().unwrap();
        assert_eq!(field_severity(ev.clone()), "warning");
        assert_eq!(field_agent_id(ev), "dashboard");
        emit_os_update_applied("device");
        assert_eq!(field_severity(rx.try_recv().unwrap()), "warning");
        emit_git_credentials_granted("a1");
        assert_eq!(field_severity(rx.try_recv().unwrap()), "warning");
    }

    fn field_severity(ev: AutopilotEvent) -> String {
        match ev {
            AutopilotEvent::SecurityEvent { severity, .. } => severity,
            other => panic!("expected SecurityEvent, got {other:?}"),
        }
    }

    fn field_agent_id(ev: AutopilotEvent) -> String {
        match ev {
            AutopilotEvent::SecurityEvent { agent_id, .. } => agent_id,
            other => panic!("expected SecurityEvent, got {other:?}"),
        }
    }
}
