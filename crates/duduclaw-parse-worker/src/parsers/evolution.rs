//! Normalize bounded event evidence in E; no paths, model calls or publication.
use duduclaw_isolation::worker::ParseError;
#[path = "../../../duduclaw-gateway/src/evolution_events/schema.rs"]
mod schema;
pub(super) fn normalize(bytes: &[u8]) -> Result<String, ParseError> {
    if bytes.len() > 8 * 1024 * 1024 { return Err(ParseError::LimitExceeded); }
    let raw = std::str::from_utf8(bytes).map_err(|_| ParseError::InvalidContent)?;
    let mut events = Vec::new();
    for line in raw.lines() {
        if line.len() > 64 * 1024 { return Err(ParseError::LimitExceeded); }
        let Ok(event) = serde_json::from_str::<schema::AuditEvent>(line.trim()) else { continue; };
        if event.event_type != schema::AuditEventType::GvuGeneration { continue; }
        let Ok(timestamp) = chrono::DateTime::parse_from_rfc3339(&event.timestamp) else { continue; };
        if event.agent_id.len() > 64 || event.skill_id.as_ref().is_some_and(|s| s.len() > 256) {
            return Err(ParseError::LimitExceeded);
        }
        if events.len() >= 1000 { return Err(ParseError::LimitExceeded); }
        events.push(serde_json::json!({"timestamp":timestamp.to_rfc3339(),"agent_id":event.agent_id,
            "effectiveness_score_delta":event.metadata.get("effectiveness_score_delta").and_then(|v| v.as_f64()).unwrap_or(0.0).clamp(0.0,1.0),
            "task_complexity":event.metadata.get("task_complexity").and_then(|v| v.as_f64()).unwrap_or(0.5).clamp(0.0,1.0),
            "is_success":event.outcome == schema::Outcome::Success,"skill_id":event.skill_id}));
    }
    serde_json::to_string(&events).map_err(|_| ParseError::InvalidContent)
}
