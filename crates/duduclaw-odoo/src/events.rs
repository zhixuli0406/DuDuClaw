//! Event bridge — Odoo changes → DuDuClaw bus events.
//!
//! [O-4a] Polling mode: periodically query Odoo for changes since last poll.
//! [O-4b] Webhook mode: receive HTTP POST from Odoo automated actions.
//! [O-4c] Routes 6 event types to bus_queue.jsonl.

use std::collections::HashMap;
use std::path::Path;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tracing::info;

use crate::connector::OdooConnector;

/// An Odoo event detected by the bridge.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OdooEvent {
    pub event_type: String,
    pub model: String,
    pub record_id: i64,
    pub data: Value,
    pub timestamp: String,
}

/// Supported event types.
pub const EVENT_TYPES: &[&str] = &[
    "odoo.crm.lead_created",
    "odoo.crm.stage_changed",
    "odoo.sale.order_confirmed",
    "odoo.sale.payment_received",
    "odoo.inventory.low_stock",
    "odoo.invoice.overdue",
];

/// Polling state tracker — stores last poll timestamp per model.
pub struct PollTracker {
    last_poll: HashMap<String, String>,
    /// Maximum records to fetch per poll. Default: 500.
    pub poll_limit: Option<usize>,
}

impl PollTracker {
    pub fn new() -> Self {
        Self { last_poll: HashMap::new(), poll_limit: None }
    }

    #[allow(dead_code)]
    pub fn with_limit(limit: usize) -> Self {
        Self { last_poll: HashMap::new(), poll_limit: Some(limit) }
    }

    /// Poll a model for records modified since last poll.
    pub async fn poll_model(
        &mut self,
        conn: &OdooConnector,
        model: &str,
        fields: &[&str],
    ) -> Result<Vec<Value>, String> {
        let since = self
            .last_poll
            .get(model)
            .cloned()
            .unwrap_or_else(|| {
                // First poll: only look back 5 minutes
                (Utc::now() - chrono::Duration::minutes(5)).format("%Y-%m-%d %H:%M:%S").to_string()
            });

        let domain = vec![json!(["write_date", ">", since])];
        // Use configurable limit; default 500 to reduce missed events (MW-M1)
        let poll_limit = self.poll_limit.unwrap_or(500);
        let result = conn.search_read(model, domain, fields, poll_limit).await?;

        // Update last poll timestamp
        self.last_poll.insert(
            model.to_string(),
            Utc::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        );

        let records = result.as_array().cloned().unwrap_or_default();
        if !records.is_empty() {
            info!(model, count = records.len(), "Polled changes from Odoo");
        }
        Ok(records)
    }
}

/// Classify a changed record into an event type.
pub fn classify_event(model: &str, record: &Value, _previous: Option<&Value>) -> Option<OdooEvent> {
    let id = record["id"].as_i64().unwrap_or(0);
    let ts = Utc::now().to_rfc3339();

    match model {
        "crm.lead" => {
            // If create_date ≈ write_date (within 2 seconds), it's a new lead (MW-M2)
            let create = record["create_date"].as_str().unwrap_or("");
            let write = record["write_date"].as_str().unwrap_or("");
            let is_new = if create.is_empty() || write.is_empty() {
                false
            } else {
                // Parse timestamps; treat as "new" if difference < 2 seconds
                chrono::NaiveDateTime::parse_from_str(create, "%Y-%m-%d %H:%M:%S")
                    .and_then(|c| {
                        chrono::NaiveDateTime::parse_from_str(write, "%Y-%m-%d %H:%M:%S")
                            .map(|w| (w - c).num_seconds().abs() < 2)
                    })
                    .unwrap_or(create == write)
            };
            let event_type = if is_new {
                "odoo.crm.lead_created"
            } else {
                "odoo.crm.stage_changed"
            };
            Some(OdooEvent {
                event_type: event_type.to_string(),
                model: model.to_string(),
                record_id: id,
                data: record.clone(),
                timestamp: ts,
            })
        }
        "sale.order" => {
            let state = record["state"].as_str().unwrap_or("");
            let event_type = match state {
                "sale" => "odoo.sale.order_confirmed",
                _ => return None,
            };
            Some(OdooEvent {
                event_type: event_type.to_string(),
                model: model.to_string(),
                record_id: id,
                data: record.clone(),
                timestamp: ts,
            })
        }
        "account.move" => {
            let payment = record["payment_state"].as_str().unwrap_or("");
            if payment == "paid" {
                Some(OdooEvent {
                    event_type: "odoo.sale.payment_received".to_string(),
                    model: model.to_string(),
                    record_id: id,
                    data: record.clone(),
                    timestamp: ts,
                })
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Write an Odoo event to bus_queue.jsonl for agent consumption.
pub fn write_event_to_bus(home_dir: &Path, event: &OdooEvent) {
    let queue_path = home_dir.join("bus_queue.jsonl");
    let entry = json!({
        "type": "odoo_event",
        "event_type": event.event_type,
        "model": event.model,
        "record_id": event.record_id,
        "data": event.data,
        "timestamp": event.timestamp,
    });

    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&queue_path)
    {
        let _ = writeln!(f, "{}", entry);
    }
}

/// Incoming webhook payload from Odoo automated action.
#[derive(Debug, Deserialize)]
pub struct WebhookPayload {
    pub event: String,
    pub model: Option<String>,
    pub record_id: Option<i64>,
    pub data: Option<Value>,
    pub secret: Option<String>,
}

/// Constant-time byte-slice equality check to prevent timing attacks.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut acc: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        acc |= x ^ y;
    }
    acc == 0
}

/// Validate and convert a webhook payload into an OdooEvent.
pub fn parse_webhook(payload: &WebhookPayload, expected_secret: &str) -> Result<OdooEvent, String> {
    // Always verify webhook secret — reject all requests if no secret configured
    if expected_secret.is_empty() {
        return Err("Webhook secret not configured — rejecting request".to_string());
    }
    let provided = payload.secret.as_deref().unwrap_or("");
    if !constant_time_eq(provided.as_bytes(), expected_secret.as_bytes()) {
        return Err("Invalid webhook secret".to_string());
    }

    Ok(OdooEvent {
        event_type: payload.event.clone(),
        model: payload.model.clone().unwrap_or_default(),
        record_id: payload.record_id.unwrap_or(0),
        data: payload.data.clone().unwrap_or(Value::Null),
        timestamp: Utc::now().to_rfc3339(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_webhook_rejects_empty_secret() {
        let payload = WebhookPayload {
            event: "test".to_string(),
            model: None,
            record_id: None,
            data: None,
            secret: None,
        };
        assert!(parse_webhook(&payload, "").is_err());
    }

    #[test]
    fn parse_webhook_rejects_wrong_secret() {
        let payload = WebhookPayload {
            event: "test".to_string(),
            model: None,
            record_id: None,
            data: None,
            secret: Some("wrong".to_string()),
        };
        assert!(parse_webhook(&payload, "correct").is_err());
    }

    #[test]
    fn parse_webhook_accepts_correct_secret() {
        let payload = WebhookPayload {
            event: "lead_created".to_string(),
            model: Some("crm.lead".to_string()),
            record_id: Some(42),
            data: None,
            secret: Some("my-secret".to_string()),
        };
        let event = parse_webhook(&payload, "my-secret").unwrap();
        assert_eq!(event.event_type, "lead_created");
        assert_eq!(event.model, "crm.lead");
        assert_eq!(event.record_id, 42);
    }

    #[test]
    fn constant_time_eq_works() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
    }

    #[test]
    fn poll_tracker_new() {
        let tracker = PollTracker::new();
        assert!(tracker.poll_limit.is_none());
    }

    #[test]
    fn poll_tracker_with_limit() {
        let tracker = PollTracker::with_limit(200);
        assert_eq!(tracker.poll_limit, Some(200));
    }
}
