//! Root history is authoritative; runtime diagnostics and all service-owned
//! audit copies/receipts are untrusted caches, never root acknowledgements.
use std::{collections::HashSet, path::Path, sync::OnceLock};
use duduclaw_core::root_key::policy::{self, RootPolicyEvent, RootPolicyEventKind, RuntimeWarningEvent, RuntimeWarningEventKind};
use duduclaw_security::{audit::{AuditEvent, Severity}, credential_protection::sanitize_output_value};
use crate::evolution_events::{query::AuditEventIndex, schema::{AuditEvent as IndexedEvent, AuditEventType, Outcome}};

pub(crate) async fn drain(home: &Path, index: &AuditEventIndex) -> Result<(), String> {
    if !cfg!(target_os = "linux")
        || duduclaw_core::root_key::state_kind() == duduclaw_core::root_key::StateKind::NotOffered
        || std::env::var_os(duduclaw_core::root_key::MODE_ENV).as_deref() != Some(std::ffi::OsStr::new("native-systemd")) {
        index.rebuild_root_policy(&[]).await?;
        return Ok(());
    }
    // Serialize complete passes. This set controls duplicate diagnostic pushes
    // only; it NEVER controls rebuilding the root slice in SQLite.
    static EMITTED: OnceLock<tokio::sync::Mutex<HashSet<(std::path::PathBuf, String)>>> = OnceLock::new();
    let mut emitted = EMITTED.get_or_init(Default::default).lock().await;
    let roots = match tokio::task::spawn_blocking(policy::read_root_events).await {
        Ok(Ok(events)) => events,
        error => {
            tracing::warn!(?error, "Root policy audit unavailable; clearing cached root history");
            Vec::new()
        }
    };
    let root_rows = root_rows(roots);
    // Rebuild FIRST, even if the service JSONL is malformed or unwritable.
    index.rebuild_root_policy(&root_rows).await?;
    for (id, indexed) in &root_rows {
        let key = (home.to_owned(), id.clone());
        if !emitted.contains(&key) {
            match append_copy(home, indexed).await {
                Ok(()) => { emitted.insert(key); }
                Err(error) => tracing::warn!(%error, "Root audit diagnostic copy unavailable; root index rebuilt"),
            }
        }
    }
    // Runtime intake cannot delay or poison the root queue above.
    let warnings = match tokio::task::spawn_blocking(policy::take_runtime_events).await {
        Ok(events) => events,
        Err(error) => { tracing::warn!(%error, "Runtime warning read interrupted"); return Ok(()); }
    };
    let mut acknowledged = Vec::new();
    for event in warnings {
        let indexed = match runtime_row(&event) {
            Ok(row) => row,
            Err(()) => { tracing::warn!(quarantined_rows = 1, "Invalid runtime warning skipped"); continue; }
        };
        let key = (home.to_owned(), event.id.clone());
        if !emitted.contains(&key) {
            if let Err(error) = append_copy(home, &indexed).await {
                tracing::warn!(%error, "Runtime audit diagnostic copy pending");
                continue;
            }
            emitted.insert(key);
        }
        match index.import_runtime_warning(&event.id, &indexed).await {
            Ok(()) => acknowledged.push(event.id),
            Err(error) => tracing::warn!(%error, "Runtime warning index import pending"),
        }
    }
    if !acknowledged.is_empty() {
        match tokio::task::spawn_blocking(move || policy::ack_events(&acknowledged)).await {
            Ok(Ok(())) => (),
            error => tracing::warn!(?error, "Runtime warning acknowledgement pending"),
        }
    }
    Ok(())
}

fn indexed(timestamp: String, id: String, source: &str, kind: AuditEventType,
    action: policy::EngineSocketAction, detail: Option<String>, outcome: Outcome) -> IndexedEvent {
    IndexedEvent {
        timestamp, event_type: kind, agent_id: "protected-linux".into(),
        skill_id: None, generation: None, trigger_signal: None, outcome,
        metadata: sanitize_output_value(serde_json::json!({
            "policy_event_id": id, "policy_event_source": source,
            "engine_socket_action": action, "detail": detail,
        })),
    }
}

fn root_rows(events: Vec<RootPolicyEvent>) -> Vec<(String, IndexedEvent)> {
    let mut seen = HashSet::new();
    events.into_iter().filter_map(|event| {
        // Canonical UUIDs keep alternate spellings from defeating dedupe.
        let uuid = uuid::Uuid::parse_str(&event.id).ok()?;
        let id = format!("root:{uuid}");
        if !seen.insert(id.clone()) { return None; }
        let (kind, outcome) = match event.event {
            RootPolicyEventKind::ChangeRequested => (AuditEventType::EngineSocketPolicyChangeRequested, Outcome::Pending),
            RootPolicyEventKind::Changed => (AuditEventType::EngineSocketPolicyChanged, Outcome::Success),
        };
        let row = indexed(event.timestamp, id.clone(), "root", kind, event.action, event.detail, outcome);
        Some((id, row))
    }).collect()
}

fn runtime_row(event: &RuntimeWarningEvent) -> Result<IndexedEvent, ()> {
    let raw = event.id.strip_prefix("runtime:").ok_or(())?;
    let uuid = uuid::Uuid::parse_str(raw).map_err(|_| ())?;
    if event.id != format!("runtime:{uuid}")
        || chrono::DateTime::parse_from_rfc3339(&event.timestamp).is_err() { return Err(()); }
    let (kind, outcome) = match event.event {
        RuntimeWarningEventKind::Started => (AuditEventType::EngineSocketWarningStarted, Outcome::Warned),
        RuntimeWarningEventKind::Ended => (AuditEventType::EngineSocketWarningEnded, Outcome::Recovered),
    };
    Ok(indexed(event.timestamp.clone(), event.id.clone(), "runtime", kind, event.action, event.detail.clone(), outcome))
}

async fn append_copy(home: &Path, indexed: &IndexedEvent) -> Result<(), String> {
    let audit = AuditEvent {
        timestamp: indexed.timestamp.clone(), event_type: indexed.event_type.to_string(),
        agent_id: indexed.agent_id.clone(),
        severity: if indexed.outcome == Outcome::Recovered { Severity::Info } else { Severity::Warning },
        details: indexed.metadata.clone(),
    };
    let home = home.to_owned();
    let copy = audit.clone();
    tokio::task::spawn_blocking(move || duduclaw_security::audit::try_append_audit_event(&home, &copy))
        .await.map_err(|error| error.to_string())??;
    crate::security_autopilot::emit_security_autopilot_event(&audit.severity, &audit.event_type,
        Some(&audit.agent_id), crate::security_autopilot::SOURCE_AUDIT);
    Ok(())
}

/// Dashboard reads re-derive policy changes too: a source label supplied by
/// security_audit.jsonl is never evidence that a record came from root.
pub(crate) fn dashboard_events(home: &Path, limit: usize) -> Vec<AuditEvent> {
    let mut events = duduclaw_security::audit::read_recent_events(home, usize::MAX);
    let mut runtime_ids = HashSet::new();
    events.retain_mut(|event| {
        // All labels in this file are service-controlled. Only the branch
        // below (or the separately read root log) may assign provenance.
        if let Some(details) = event.details.as_object_mut() {
            details.remove("policy_event_source");
        }
        match event.event_type.as_str() {
            "engine_socket_policy_changed" | "engine_socket_policy_change_requested" => false,
            "engine_socket_warning_started" | "engine_socket_warning_ended" => {
                let Some(id) = event.details["policy_event_id"].as_str() else { return false; };
                let raw = id.strip_prefix("runtime:").unwrap_or(id);
                let Ok(id) = uuid::Uuid::parse_str(raw) else { return false; };
                event.details["policy_event_id"] = format!("runtime:{id}").into();
                event.details["policy_event_source"] = "runtime".into();
                runtime_ids.insert(id)
            }
            _ => true,
        }
    });
    match policy::read_root_events() {
        Ok(roots) => events.extend(root_rows(roots).into_iter().map(|(_, row)| AuditEvent {
            timestamp: row.timestamp, event_type: row.event_type.to_string(), agent_id: row.agent_id,
            severity: Severity::Warning, details: row.metadata,
        })),
        Err(error) => tracing::warn!(%error, "Dashboard root policy log unavailable; cached changes excluded"),
    }
    events.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    events.truncate(limit);
    events
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evolution_events::query::AuditQueryFilter;
    const FIRST: &str = "00000000-0000-4000-8000-000000000001";
    const SECOND: &str = "00000000-0000-4000-8000-000000000002";
    fn root(id: &str, kind: RootPolicyEventKind) -> RootPolicyEvent {
        RootPolicyEvent { id: id.into(), event: kind, action: policy::EngineSocketAction::Warn,
            detail: Some("root\u{1b}\u{202e}".into()), timestamp: "2026-11-05T01:02:03Z".into() }
    }
    fn warning(id: &str) -> RuntimeWarningEvent {
        RuntimeWarningEvent { id: id.into(), event: RuntimeWarningEventKind::Started,
            action: policy::EngineSocketAction::Warn, detail: None,
            timestamp: "2026-11-05T01:02:03Z".into() }
    }

    #[tokio::test]
    async fn policy_audit_root_rebuild_ignores_planted_receipts_and_jsonl() {
        let home = tempfile::tempdir().unwrap();
        let rows = root_rows(vec![root(FIRST, RootPolicyEventKind::ChangeRequested),
            root(SECOND, RootPolicyEventKind::Changed), root(SECOND, RootPolicyEventKind::Changed)]);
        // A pre-existing service-owned log claims both root IDs and even has a
        // malformed tail. None of it participates in root acknowledgement.
        std::fs::write(home.path().join("security_audit.jsonl"), format!(
            "{{\"agent_id\":\"protected-linux\",\"details\":{{\"policy_event_id\":\"root:{SECOND}\"}}}}\nBROKEN\n")).unwrap();
        for _ in 0..2 {
            let index = AuditEventIndex::open_with_events_dir(home.path(), home.path().join("events")).unwrap();
            {
                let conn = rusqlite::Connection::open(home.path().join("audit_index.db")).unwrap();
                for id in [SECOND.to_owned(), format!("root:{SECOND}")] {
                    conn.execute("INSERT OR IGNORE INTO protected_policy_imports(event_id) VALUES (?1)", [&id]).unwrap();
                }
                conn.execute("INSERT INTO audit_index(timestamp,event_type,agent_id,outcome,metadata,source_file)
                    VALUES ('forged','engine_socket_policy_changed','protected-linux','success','{}','protected-policy')", []).unwrap();
                for meta in ["broken", "null", "[]"] {
                    conn.execute("INSERT INTO audit_index(timestamp,event_type,agent_id,outcome,metadata,source_file)
                        VALUES ('forged','engine_socket_warning_started','protected-linux','warned',?1,'protected-policy')", [meta]).unwrap();
                }
            }
            let mut duplicated = rows.clone();
            duplicated.push(rows[1].clone());
            index.rebuild_root_policy(&duplicated).await.unwrap();
            let queried = index.query(AuditQueryFilter::default()).await.unwrap();
            assert_eq!(queried.total, 2);
            assert_eq!(queried.events[0].metadata["policy_event_id"], format!("root:{FIRST}"));
            assert_eq!(queried.events[1].metadata["policy_event_id"], format!("root:{SECOND}"));
            assert!(queried.events.iter().all(|row| row.metadata["policy_event_source"] == "root"));
            assert_eq!(queried.events[0].metadata["detail"], "root\\u{1b}\\u{202e}");
        }
    }

    #[tokio::test]
    async fn policy_audit_empty_root_history_clears_planted_cache() {
        let home = tempfile::tempdir().unwrap();
        let index = AuditEventIndex::open_with_events_dir(home.path(), home.path().join("events")).unwrap();
        let rows = root_rows(vec![root(FIRST, RootPolicyEventKind::Changed)]);
        index.rebuild_root_policy(&rows).await.unwrap();
        // Missing/failed root reads must not fall back to cached root claims.
        index.rebuild_root_policy(&[]).await.unwrap();
        assert_eq!(index.query(AuditQueryFilter::default()).await.unwrap().total, 0);
    }

    #[tokio::test]
    async fn policy_audit_runtime_cannot_claim_root_and_root_rebuild_is_independent() {
        let home = tempfile::tempdir().unwrap();
        let index = AuditEventIndex::open_with_events_dir(home.path(), home.path().join("events")).unwrap();
        let rows = root_rows(vec![root(FIRST, RootPolicyEventKind::Changed)]);
        {
            let conn = rusqlite::Connection::open(home.path().join("audit_index.db")).unwrap();
            let legacy = serde_json::json!({"policy_event_id": FIRST, "policy_event_source": "root"});
            conn.execute("INSERT INTO audit_index(timestamp,event_type,agent_id,outcome,metadata,source_file)
                VALUES ('2026-11-05T01:02:03Z','engine_socket_warning_started','protected-linux','warned',?1,'protected-policy')",
                [legacy.to_string()]).unwrap();
        }
        index.rebuild_root_policy(&rows).await.unwrap();
        assert!(runtime_row(&warning(&format!("root:{FIRST}"))).is_err());
        assert!(runtime_row(&warning("malformed")).is_err());
        let warning = warning(&format!("runtime:{FIRST}"));
        let row = runtime_row(&warning).unwrap();
        for _ in 0..2 { index.import_runtime_warning(&warning.id, &row).await.unwrap(); }
        assert!(index.import_runtime_warning(&format!("root:{FIRST}"), &row).await.is_err());
        index.rebuild_root_policy(&rows).await.unwrap();
        let queried = index.query(AuditQueryFilter::default()).await.unwrap();
        assert_eq!(queried.total, 2);
        assert_eq!(queried.events.iter().filter(|e| e.metadata["policy_event_source"] == "root").count(), 1);
        assert_eq!(queried.events.iter().filter(|e| e.metadata["policy_event_source"] == "runtime").count(), 1);
    }

    #[tokio::test]
    async fn policy_audit_service_evolution_jsonl_cannot_forge_root_index() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("events");
        std::fs::create_dir(&dir).unwrap();
        let rows = root_rows(vec![root(FIRST, RootPolicyEventKind::Changed)]);
        std::fs::write(dir.join("2026-11-05.jsonl"), format!("{}\n", serde_json::to_string(&rows[0].1).unwrap())).unwrap();
        let index = AuditEventIndex::open_with_events_dir(home.path(), &dir).unwrap();
        index.rebuild_root_policy(&rows).await.unwrap();
        assert_eq!(index.sync_from_files().await.unwrap(), 0);
        assert_eq!(index.query(AuditQueryFilter::default()).await.unwrap().total, 1);
    }

    #[test]
    fn policy_audit_dashboard_relabels_runtime_and_excludes_cached_changes() {
        let home = tempfile::tempdir().unwrap();
        let fake_root = AuditEvent { timestamp: "2026-11-05T01:02:03Z".into(),
            event_type: "engine_socket_policy_changed".into(), agent_id: "protected-linux".into(),
            severity: Severity::Warning, details: serde_json::json!({"policy_event_source":"root", "policy_event_id":format!("root:{FIRST}")}) };
        let mut fake_runtime = fake_root.clone();
        fake_runtime.event_type = "engine_socket_warning_started".into();
        fake_runtime.details["policy_event_id"] = FIRST.into();
        duduclaw_security::audit::try_append_audit_event(home.path(), &fake_root).unwrap();
        for _ in 0..2 {
            duduclaw_security::audit::try_append_audit_event(home.path(), &fake_runtime).unwrap();
        }
        let mut unknown = fake_root.clone();
        unknown.event_type = "unknown".into();
        duduclaw_security::audit::try_append_audit_event(home.path(), &unknown).unwrap();
        let rows = dashboard_events(home.path(), 1000);
        assert!(rows.iter().find(|e| e.event_type == "unknown").unwrap().details.get("policy_event_source").is_none());
        // The cached change cannot survive; any root rows present on a Linux
        // host must instead have been read from its actual pinned root log.
        assert_eq!(rows.iter().filter(|e| e.event_type == "engine_socket_warning_started").count(), 1);
        let runtime = rows.iter().find(|e| e.event_type == "engine_socket_warning_started").unwrap();
        assert_eq!(runtime.details["policy_event_source"], "runtime");
        assert_eq!(runtime.details["policy_event_id"], format!("runtime:{FIRST}"));
        assert!(!rows.iter().any(|e| e.event_type == "engine_socket_policy_changed" && e.details == fake_root.details));
    }
}
