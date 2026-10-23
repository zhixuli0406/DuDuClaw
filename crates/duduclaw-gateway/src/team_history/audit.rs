//! A successful disclosure requires a durable, chained audit record.
use duduclaw_security::audit::{AuditEvent, Severity, try_append_audit_event};
use serde_json::Value;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

pub fn session_digest(home: &Path, id: &str) -> Result<String, String> {
    let digest = crate::trigger_route::access::keyed_digest(home, "team_history_session", id);
    if digest == "unavailable" {
        Err("audit_unavailable".into())
    } else {
        Ok(digest)
    }
}

/// Deduplication is committed only after a successful append. A failed
/// writer never poisons a later retry into skipping the mandatory audit.
pub fn write(
    home: &Path,
    event_type: &str,
    viewer: &str,
    agent: &str,
    details: Value,
    dedup: Option<String>,
) -> Result<(), String> {
    static SEEN: OnceLock<Mutex<HashMap<(PathBuf, String, String, String), i64>>> = OnceLock::new();
    let now = chrono::Utc::now().timestamp();
    let mut seen = SEEN
        .get_or_init(Default::default)
        .lock()
        .map_err(|_| "audit_unavailable")?;
    seen.retain(|_, t| *t > now - 60);
    let key = dedup.map(|d| (home.to_path_buf(), viewer.into(), event_type.into(), d));
    if key.as_ref().is_some_and(|k| seen.contains_key(k)) {
        return Ok(());
    }
    let severity = if event_type == "team_history_denied" {
        Severity::Warning
    } else {
        Severity::Info
    };
    try_append_audit_event(home, &AuditEvent::new(event_type, agent, severity, details))
        .map_err(|_| "audit_unavailable")?;
    if let Some(key) = key {
        if seen.len() < 10_000 {
            seen.insert(key, now);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn failure_does_not_mark_a_deduplicated_audit_as_written() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir(home.path().join("security_audit.jsonl")).unwrap();
        assert!(
            write(
                home.path(),
                "team_history_listed",
                "manager",
                "sales",
                serde_json::json!({}),
                Some("same".into())
            )
            .is_err()
        );
        std::fs::remove_dir(home.path().join("security_audit.jsonl")).unwrap();
        write(
            home.path(),
            "team_history_listed",
            "manager",
            "sales",
            serde_json::json!({}),
            Some("same".into()),
        )
        .unwrap();
        assert!(
            std::fs::read_to_string(home.path().join("security_audit.jsonl"))
                .unwrap()
                .contains("team_history_listed")
        );
    }
}
