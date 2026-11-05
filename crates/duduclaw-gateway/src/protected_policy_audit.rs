//! Import verified root policy records and runtime warning transitions.
//! These audit copies are diagnostic only; they never grant policy authority.
use std::{collections::{HashSet, VecDeque}, io::{BufRead, BufReader}, path::Path, sync::OnceLock};
use duduclaw_core::root_key::policy::PolicyEvent;
use duduclaw_security::{audit::{AuditEvent, Severity}, credential_protection::sanitize_output_value};
use crate::evolution_events::{query::AuditEventIndex, schema::{AuditEvent as IndexedEvent, AuditEventType, Outcome}};

pub(crate) async fn drain(home: &Path, index: &AuditEventIndex) {
    if !cfg!(target_os = "linux")
        || duduclaw_core::root_key::state_kind() == duduclaw_core::root_key::StateKind::NotOffered
        || std::env::var_os(duduclaw_core::root_key::MODE_ENV).as_deref() != Some(std::ffi::OsStr::new("native-systemd")) {
        return;
    }
    static PENDING: OnceLock<tokio::sync::Mutex<VecDeque<PolicyEvent>>> = OnceLock::new();
    let mut pending = PENDING.get_or_init(Default::default).lock().await;
    let events = match tokio::task::spawn_blocking(duduclaw_core::root_key::policy::take_events).await {
        Ok(events) => events,
        Err(error) => { tracing::warn!(%error, "Protected policy audit read interrupted"); return; }
    };
    enqueue_unique(&mut pending, events);
    let mut acknowledged = Vec::new();
    let result = drain_pending(home, index, &mut pending, &mut acknowledged).await;
    // A later failure must not leave already-imported transitions unacknowledged.
    // Failed acknowledgements are retried through the durable receipt fast path.
    if !acknowledged.is_empty() {
        match tokio::task::spawn_blocking(move || duduclaw_core::root_key::policy::ack_events(&acknowledged)).await {
            Ok(Ok(())) => (),
            error => tracing::warn!(?error, "Protected policy journal acknowledgement pending"),
        }
    }
    if let Err(error) = result {
        tracing::warn!(error = %duduclaw_core::root_key::recovery::sanitize_diagnostic(&error),
            "Protected policy audit import pending; will retry");
    }
}

fn event_type(name: &str) -> Result<AuditEventType, String> {
    match name {
        "engine_socket_policy_change_requested" => Ok(AuditEventType::EngineSocketPolicyChangeRequested),
        "engine_socket_policy_changed" => Ok(AuditEventType::EngineSocketPolicyChanged),
        "engine_socket_warning_started" => Ok(AuditEventType::EngineSocketWarningStarted),
        "engine_socket_warning_ended" => Ok(AuditEventType::EngineSocketWarningEnded),
        _ => Err("unknown protected policy audit event".into()),
    }
}

fn enqueue_unique(pending: &mut VecDeque<PolicyEvent>, events: Vec<PolicyEvent>) {
    let mut ids: HashSet<String> = pending.iter().map(|event| event.id.clone()).collect();
    pending.extend(events.into_iter().filter(|event| ids.insert(event.id.clone())));
}

fn logged_ids(home: &Path, wanted: &HashSet<String>) -> Result<HashSet<String>, String> {
    let mut found = HashSet::new();
    let file = match std::fs::File::open(home.join("security_audit.jsonl")) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(found),
        Err(error) => return Err(error.to_string()),
    };
    // Scan at most once per pending batch, on the blocking pool. Completed
    // receipts skip this scan entirely, including when root history is replayed.
    for line in BufReader::new(file).lines() {
        let line = line.map_err(|e| e.to_string())?;
        if let Ok(event) = serde_json::from_str::<AuditEvent>(&line) {
            if event.agent_id == "protected-linux" {
                if let Some(id) = event.details["policy_event_id"].as_str().filter(|id| wanted.contains(*id)) {
                    found.insert(id.to_owned());
                    if found.len() == wanted.len() { break; }
                }
            }
        }
    }
    Ok(found)
}

async fn drain_pending(home: &Path, index: &AuditEventIndex, pending: &mut VecDeque<PolicyEvent>,
    acknowledged: &mut Vec<String>) -> Result<(), String> {
    if pending.is_empty() { return Ok(()); }
    let ids: Vec<_> = pending.iter().map(|event| event.id.clone()).collect();
    let imported = index.imported_protected_policy_ids(&ids).await?;
    pending.retain(|event| {
        if imported.contains(&event.id) { acknowledged.push(event.id.clone()); false } else { true }
    });
    if pending.is_empty() { return Ok(()); }
    let wanted: HashSet<_> = pending.iter().map(|event| event.id.clone()).collect();
    let home_owned = home.to_owned();
    let mut logged = tokio::task::spawn_blocking(move || logged_ids(&home_owned, &wanted))
        .await.map_err(|error| error.to_string())??;
    while let Some(event) = pending.front() {
        let kind = event_type(&event.event)?;
        let ended = event.event == "engine_socket_warning_ended";
        let requested = event.event == "engine_socket_policy_change_requested";
        let metadata = sanitize_output_value(serde_json::json!({
            "policy_event_id": event.id, "engine_socket_action": event.action, "detail": event.detail,
        }));
        let indexed = IndexedEvent {
            timestamp: event.timestamp.clone(), event_type: kind, agent_id: "protected-linux".into(),
            skill_id: None, generation: None, trigger_signal: None,
            outcome: if ended { Outcome::Recovered } else if requested { Outcome::Pending }
                else if event.event == "engine_socket_warning_started" { Outcome::Warned } else { Outcome::Success },
            metadata: metadata.clone(),
        };
        // Do not consume the queue until BOTH durable copies exist. Stable ids
        // make a retry safe if the process stops between the two writes.
        if !logged.contains(&event.id) {
            let audit = AuditEvent { timestamp: event.timestamp.clone(), event_type: event.event.clone(),
                agent_id: "protected-linux".into(), severity: if ended { Severity::Info } else { Severity::Warning },
                details: metadata };
            let home_owned = home.to_owned();
            let audit_copy = audit.clone();
            tokio::task::spawn_blocking(move || duduclaw_security::audit::try_append_audit_event(&home_owned, &audit_copy))
                .await.map_err(|error| error.to_string())??;
            logged.insert(event.id.clone());
            crate::security_autopilot::emit_security_autopilot_event(&audit.severity, &audit.event_type,
                Some(&audit.agent_id), crate::security_autopilot::SOURCE_AUDIT);
        }
        index.import_protected_policy(&event.id, &indexed).await?;
        acknowledged.push(event.id.clone());
        pending.pop_front();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use duduclaw_core::root_key::policy::EngineSocketAction;
    use crate::evolution_events::query::AuditQueryFilter;

    fn event(id: &str, name: &str) -> PolicyEvent {
        PolicyEvent { id: id.into(), event: name.into(), action: EngineSocketAction::Warn,
            detail: Some("socket\u{1b}\u{202e}".into()), timestamp: "2026-11-05T01:02:03Z".into() }
    }

    #[tokio::test]
    async fn policy_audit_import_is_idempotent_across_reopen_and_sanitizes() {
        let home = tempfile::tempdir().unwrap();
        let events = vec![event("request", "engine_socket_policy_change_requested"),
            event("change", "engine_socket_policy_changed"), event("start", "engine_socket_warning_started"),
            event("end", "engine_socket_warning_ended")];
        for _ in 0..2 {
            let index = AuditEventIndex::open_with_events_dir(home.path(), home.path().join("events")).unwrap();
            let mut pending = VecDeque::from(events.clone());
            drain_pending(home.path(), &index, &mut pending, &mut Vec::new()).await.unwrap();
            assert!(pending.is_empty());
            let rows = index.query(AuditQueryFilter::default()).await.unwrap();
            assert_eq!(rows.total, 4);
            assert_eq!(rows.events.len(), 4);
        }
        let audits = duduclaw_security::audit::read_recent_events(home.path(), 10);
        assert_eq!(audits.len(), 4);
        assert_eq!(audits[0].details["detail"], "socket\\u{1b}\\u{202e}");
    }

    #[tokio::test]
    async fn policy_audit_failure_retains_transition_for_retry() {
        let home = tempfile::tempdir().unwrap();
        let index = AuditEventIndex::open_with_events_dir(home.path(), home.path().join("events")).unwrap();
        std::fs::create_dir(home.path().join("security_audit.jsonl")).unwrap();
        let mut pending = VecDeque::from([event("retry", "engine_socket_warning_started")]);
        assert!(drain_pending(home.path(), &index, &mut pending, &mut Vec::new()).await.is_err());
        assert_eq!(pending.len(), 1);
        std::fs::remove_dir(home.path().join("security_audit.jsonl")).unwrap();
        drain_pending(home.path(), &index, &mut pending, &mut Vec::new()).await.unwrap();
        assert!(pending.is_empty());
    }

    #[test]
    fn policy_audit_pending_journal_replay_deduplicates_ids() {
        let first = event("first", "engine_socket_warning_started");
        let second = event("second", "engine_socket_warning_ended");
        let mut pending = VecDeque::from([first.clone()]);
        enqueue_unique(&mut pending, vec![first, second.clone(), second]);
        assert_eq!(pending.iter().map(|event| event.id.as_str()).collect::<Vec<_>>(), vec!["first", "second"]);
    }

    #[tokio::test]
    async fn policy_audit_receipt_replay_skips_log_read_and_retries_acknowledgement() {
        let home = tempfile::tempdir().unwrap();
        let index = AuditEventIndex::open_with_events_dir(home.path(), home.path().join("events")).unwrap();
        let record = event("done", "engine_socket_warning_started");
        let mut pending = VecDeque::from([record.clone()]);
        let mut acknowledged = Vec::new();
        drain_pending(home.path(), &index, &mut pending, &mut acknowledged).await.unwrap();
        assert_eq!(acknowledged, vec!["done"]);
        std::fs::rename(home.path().join("security_audit.jsonl"), home.path().join("saved.jsonl")).unwrap();
        std::fs::create_dir(home.path().join("security_audit.jsonl")).unwrap();
        let mut pending = VecDeque::from([record]);
        acknowledged.clear();
        drain_pending(home.path(), &index, &mut pending, &mut acknowledged).await.unwrap();
        assert!(pending.is_empty());
        assert_eq!(acknowledged, vec!["done"]);
    }

    #[tokio::test]
    async fn policy_audit_partial_batch_acknowledges_only_completed_records() {
        let home = tempfile::tempdir().unwrap();
        let index = AuditEventIndex::open_with_events_dir(home.path(), home.path().join("events")).unwrap();
        let mut pending = VecDeque::from([event("done", "engine_socket_warning_started"), event("bad", "unknown")]);
        let mut acknowledged = Vec::new();
        assert!(drain_pending(home.path(), &index, &mut pending, &mut acknowledged).await.is_err());
        assert_eq!(acknowledged, vec!["done"]);
        assert_eq!(pending.front().unwrap().id, "bad");
    }

}
