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
}

impl PollTracker {
    pub fn new() -> Self {
        Self {
            last_poll: HashMap::new(),
        }
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
        let result = conn.search_read(model, domain, fields, 100).await?;

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
            // If create_date == write_date (approximately), it's a new lead
            let create = record["create_date"].as_str().unwrap_or("");
            let write = record["write_date"].as_str().unwrap_or("");
            let event_type = if create == write {
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

/// Validate and convert a webhook payload into an OdooEvent.
pub fn parse_webhook(payload: &WebhookPayload, expected_secret: &str) -> Result<OdooEvent, String> {
    // Verify secret if configured
    if !expected_secret.is_empty() {
        let provided = payload.secret.as_deref().unwrap_or("");
        if provided != expected_secret {
            return Err("Invalid webhook secret".to_string());
        }
    }

    Ok(OdooEvent {
        event_type: payload.event.clone(),
        model: payload.model.clone().unwrap_or_default(),
        record_id: payload.record_id.unwrap_or(0),
        data: payload.data.clone().unwrap_or(Value::Null),
        timestamp: Utc::now().to_rfc3339(),
    })
}
