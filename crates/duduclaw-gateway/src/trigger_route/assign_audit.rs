//! Audit trail of chat assignments: `human_assign` (Info) for every created
//! task, `human_assign_refused` (Warning, at most once per sender, employee
//! and reason per minute) for refusals. The sender is the dashboard account
//! when mapped, else an HMAC of the channel id ([`SenderAccess::audit_label`]).

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use duduclaw_security::audit::{AuditEvent, Severity, append_audit_event};

use super::assign::AssignCaller;
use super::access::SenderAccess;

const WINDOW: Duration = Duration::from_secs(60);
const MAX: usize = 4_096;

/// A refused `/交辦`. `reason`: `needs_identity`, `call_only`, `denied`.
pub(crate) fn refused(caller: &AssignCaller<'_>, agent: &str, reason: &'static str) {
    let home = caller.home;
    let who = label(home, caller, &caller.access);
    tracing::info!(channel = caller.channel, sender = %who, agent, reason, "chat assignment refused");
    if recently(&who, agent, reason) {
        return;
    }
    let event = AuditEvent::new(
        "human_assign_refused",
        agent,
        Severity::Warning,
        serde_json::json!({ "channel": caller.channel, "sender": who, "reason": reason }),
    );
    append_audit_event(home, &event);
}

/// A task created by `/交辦`.
pub(crate) fn assigned(caller: &AssignCaller<'_>, agent: &str, task_id: &str, via: &str) {
    let home = caller.home;
    let who = label(home, caller, &caller.access);
    tracing::info!(channel = caller.channel, sender = %who, agent, task = task_id, via, "chat assignment created");
    let event = AuditEvent::new(
        "human_assign",
        agent,
        Severity::Info,
        serde_json::json!({
            "channel": caller.channel,
            "sender": who,
            "task_id": task_id,
            "via": via,
        }),
    );
    append_audit_event(home, &event);
}

fn label(home: &std::path::Path, caller: &AssignCaller<'_>, access: &SenderAccess) -> String {
    access.audit_label(home, caller.channel, caller.sender)
}

fn recently(who: &str, agent: &str, reason: &str) -> bool {
    type Key = (String, String, String);
    static SEEN: OnceLock<Mutex<HashMap<Key, Instant>>> = OnceLock::new();
    let mut seen = SEEN
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let now = Instant::now();
    if seen.len() >= MAX {
        seen.retain(|_, t| now.saturating_duration_since(*t) < WINDOW);
        if seen.len() >= MAX {
            seen.clear();
        }
    }
    let key = (who.to_string(), agent.to_string(), reason.to_string());
    match seen.get(&key) {
        Some(t) if now.saturating_duration_since(*t) < WINDOW => true,
        _ => {
            seen.insert(key, now);
            false
        }
    }
}
