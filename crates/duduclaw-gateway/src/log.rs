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

/// Install the full gateway tracing stack: env filter (`RUST_LOG` →
/// `config.toml [general] log_level` → `"warn"`), stderr fmt layer,
/// daily-rolling file layer under `<home>/logs/`, the WebSocket
/// [`BroadcastLayer`], and the optional OTel bridge.
///
/// The open CLI installs an equivalent stack in `duduclaw-cli::entry_point`
/// (with CLI-specific stdout hygiene notes); this shared entry exists for
/// embedders that boot `start_gateway` directly. The `duduclaw-pro` binary
/// shipped with a stub that installed NOTHING — every `info!`/`warn!` in the
/// gateway was silently dropped, which kept the 2026-08 experiment
/// container's scheduler death invisible (`docker logs` empty, dashboard log
/// stream empty, file log absent). Any gateway embedder MUST call this (or
/// install its own subscriber) before `start_gateway`.
///
/// Best-effort and idempotent: an unwritable log dir degrades to stderr-only,
/// and a second call (or a subscriber installed elsewhere) is a no-op via
/// `try_init`.
pub fn init_tracing_stack(home_dir: &std::path::Path) {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    // Persistent file log — same fallible builder as the CLI (2026-07-28
    // incident: `rolling::daily` panics on an unwritable dir).
    let log_dir = home_dir.join("logs");
    let _ = std::fs::create_dir_all(&log_dir);
    let file_writer = match tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix("gateway.log")
        .build(&log_dir)
    {
        Ok(appender) => {
            let (non_blocking, guard) = tracing_appender::non_blocking(appender);
            // Process-lifetime writer: leak the guard so it never flushes and
            // closes the file early (mirrors the CLI).
            std::mem::forget(guard);
            Some(non_blocking)
        }
        Err(e) => {
            eprintln!("[duduclaw] file log disabled ({e}) — continuing with stderr logging only");
            None
        }
    };

    // Three-tier level resolution: RUST_LOG → config.toml → "warn".
    let config_level = std::fs::read_to_string(home_dir.join("config.toml"))
        .ok()
        .and_then(|c| c.parse::<toml::Table>().ok())
        .and_then(|t| {
            t.get("general")?
                .as_table()?
                .get("log_level")?
                .as_str()
                .map(str::to_string)
        });
    let (env_filter, level_source) = match std::env::var("RUST_LOG") {
        Ok(spec) => (
            tracing_subscriber::EnvFilter::try_new(&spec)
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
            format!("RUST_LOG={spec}"),
        ),
        Err(_) => match config_level {
            Some(level) => (
                tracing_subscriber::EnvFilter::try_new(&level)
                    .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
                format!("config.toml [general] log_level={level}"),
            ),
            None => (
                tracing_subscriber::EnvFilter::new("warn"),
                "default=warn".to_string(),
            ),
        },
    };
    eprintln!("[duduclaw] effective log level: {level_source}");

    // OTel must init before `subscriber_layer()` (the bridge needs the
    // installed provider). Guard is process-lifetime here, so leak it.
    if let Some(guard) = crate::otel::init(home_dir) {
        std::mem::forget(guard);
    }
    let file_layer = file_writer.map(|w| {
        tracing_subscriber::fmt::layer()
            .with_ansi(false)
            .with_writer(w)
    });
    let _ = tracing_subscriber::registry()
        .with(env_filter)
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .with(file_layer)
        .with(BroadcastLayer)
        .with(crate::otel::subscriber_layer())
        .try_init();
}

/// A `tracing_subscriber::Layer` that captures events and pushes them as
/// JSON lines to the broadcast channel.
pub struct BroadcastLayer;

impl<S: Subscriber> Layer<S> for BroadcastLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let level = match *event.metadata().level() {
            Level::ERROR => "error",
            Level::WARN => "warn",
            Level::INFO => "info",
            Level::DEBUG => "debug",
            Level::TRACE => "trace",
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
    // WP12: prefix scanning misses credentials embedded in a URL *path* — a
    // Telegram error prints `…/bot<token>/getMe`, which none of the prefixes
    // above match. Run the shape-driven redactor first.
    let mut result = crate::secret_redact::redact_secrets(msg).into_owned();
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
