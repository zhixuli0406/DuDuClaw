//! Tracing layer that broadcasts structured log events over WebSocket.
//!
//! Call [`BroadcastLayer::new`] to create the layer, then pass the returned
//! [`broadcast::Sender<String>`] to [`crate::server::AppState`] so that
//! connected `logs.subscribe` clients receive events in real-time.

use std::sync::OnceLock;

use tokio::sync::broadcast;
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::Context;
use tracing_subscriber::Layer;

/// Global sender initialised once in [`init_log_broadcaster`].
static LOG_TX: OnceLock<broadcast::Sender<String>> = OnceLock::new();

/// Initialise the global broadcaster and return the sender.
///
/// Call this once at startup (before any subscribers connect).
pub fn init_log_broadcaster() -> broadcast::Sender<String> {
    let (tx, _) = broadcast::channel::<String>(512);
    let _ = LOG_TX.set(tx.clone());
    tx
}

/// Return a clone of the global sender (if already initialised).
pub fn log_sender() -> Option<broadcast::Sender<String>> {
    LOG_TX.get().cloned()
}

/// Push a raw JSON log line to all subscribers.
///
/// Used by channel bots and other components that want to surface events.
pub fn push_log(level: &str, target: &str, message: &str) {
    if let Some(tx) = LOG_TX.get() {
        let line = serde_json::json!({
            "level": level,
            "target": target,
            "message": message,
            "timestamp": chrono::Utc::now().to_rfc3339(),
        })
        .to_string();
        let _ = tx.send(line);
    }
}

/// A `tracing_subscriber::Layer` that captures events and pushes them as
/// JSON lines to the broadcast channel.
pub struct BroadcastLayer;

impl<S: Subscriber> Layer<S> for BroadcastLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let level = match *event.metadata().level() {
            Level::ERROR => "ERROR",
            Level::WARN => "WARN",
            Level::INFO => "INFO",
            Level::DEBUG => "DEBUG",
            Level::TRACE => "TRACE",
        };

        // Capture the message field from the event
        let mut visitor = MessageVisitor { message: String::new() };
        event.record(&mut visitor);

        if visitor.message.is_empty() {
            return; // Skip events with no message
        }

        // Scrub messages that might contain sensitive data before broadcasting (BE-M5)
        let msg = scrub_sensitive(&visitor.message);
        push_log(level, event.metadata().target(), &msg);
    }
}

/// Redact values that look like secrets from log messages before broadcast.
///
/// Handles multiple occurrences of the same prefix and end-of-string values.
fn scrub_sensitive(msg: &str) -> String {
    let sensitive_prefixes = [
        "api_key=", "token=", "secret=", "password=", "credential=",
        "Bearer ", "Bot ", "ANTHROPIC_API_KEY=",
    ];
    let mut result = msg.to_string();
    for prefix in &sensitive_prefixes {
        // Loop to handle multiple occurrences of the same prefix
        let mut search_from = 0;
        while let Some(rel_pos) = result[search_from..].find(prefix) {
            let pos = search_from + rel_pos;
            let value_start = pos + prefix.len();
            if value_start >= result.len() {
                break; // prefix at very end, nothing to redact
            }
            let value_end = result[value_start..]
                .find(|c: char| c.is_whitespace() || c == ',' || c == '"' || c == '\'' || c == '}' || c == ']')
                .map(|i| value_start + i)
                .unwrap_or(result.len());
            if value_end > value_start {
                result.replace_range(value_start..value_end, "****");
                search_from = value_start + 4; // skip past "****"
            } else {
                break;
            }
        }
    }
    result
}

/// Minimal visitor that extracts the `message` field from a tracing event.
struct MessageVisitor {
    message: String,
}

impl tracing::field::Visit for MessageVisitor {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            self.message = value.to_string();
        }
    }

    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{value:?}");
        }
    }
}
