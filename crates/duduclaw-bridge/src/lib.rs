use pyo3::prelude::*;

/// Returns the version of the native bridge.
#[pyfunction]
fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Get the DuDuClaw home directory (canonical `DUDUCLAW_HOME`-aware resolver).
fn get_duduclaw_home() -> std::path::PathBuf {
    duduclaw_core::duduclaw_home()
}

/// Maximum payload size for bus queue entries (1 MB).
const MAX_PAYLOAD_SIZE: usize = 1_048_576;

/// Allowed channel types for incoming messages.
const ALLOWED_CHANNELS: &[&str] = &["telegram", "line", "discord"];

/// Maximum length for a channel `chat_id` (covers all supported channels).
const MAX_CHAT_ID_LEN: usize = 256;
/// Maximum length for a `sender` identifier/display name.
const MAX_SENDER_LEN: usize = 256;
/// Maximum length for incoming message `text`.
const MAX_TEXT_LEN: usize = 32_768;

/// Validate agent ID is safe for filesystem paths (no traversal).
fn is_valid_agent_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !id.starts_with('-')
        && !id.ends_with('-')
}

/// Write a JSON line to the bus queue file for the Rust gateway to process.
fn write_to_queue(msg: &serde_json::Value) -> std::io::Result<()> {
    let serialized = msg.to_string();
    if serialized.len() > MAX_PAYLOAD_SIZE {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("Payload exceeds maximum size ({MAX_PAYLOAD_SIZE} bytes)"),
        ));
    }
    let queue_path = get_duduclaw_home().join("bus_queue.jsonl");
    // Serialize concurrent appends across processes (Rust gateway + Python
    // adapters). With MAX_PAYLOAD_SIZE (1 MiB) >> PIPE_BUF, plain append writes
    // can interleave into malformed JSONL that the consumer silently drops.
    duduclaw_core::with_file_lock(&queue_path, || {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&queue_path)?;
        use std::io::Write;
        writeln!(file, "{}", serialized)
    })
}

/// Send a message to a specific agent via the Rust core bus.
///
/// The message is written to `~/.duduclaw/bus_queue.jsonl` which the
/// running gateway polls to deliver cross-agent messages.
#[pyfunction]
fn send_message(agent_id: &str, payload: &str) -> PyResult<String> {
    if !is_valid_agent_id(agent_id) {
        return Err(pyo3::exceptions::PyValueError::new_err(
            "agent_id must be lowercase alphanumeric with hyphens, max 64 chars",
        ));
    }

    let msg_id = format!("{}", std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros());

    let msg = serde_json::json!({
        "type": "agent_message",
        "message_id": &msg_id,
        "agent_id": agent_id,
        "payload": payload,
        "timestamp": chrono::Utc::now().to_rfc3339(),
    });

    write_to_queue(&msg)
        .map_err(|e| pyo3::exceptions::PyIOError::new_err(format!("Failed to write to bus queue: {e}")))?;

    Ok(msg_id)
}

/// Route an incoming channel message to the Rust processing bus.
///
/// Called by Python channel plugins (Telegram/LINE/Discord) when a message
/// arrives. Writes to `~/.duduclaw/bus_queue.jsonl` for gateway pickup.
#[pyfunction]
fn send_to_bus(channel: &str, chat_id: &str, sender: &str, text: &str) -> PyResult<()> {
    // Validate channel is in the allowed whitelist
    if !ALLOWED_CHANNELS.contains(&channel) {
        return Err(pyo3::exceptions::PyValueError::new_err(
            format!("Unknown channel: {channel}. Allowed: {}", ALLOWED_CHANNELS.join(", ")),
        ));
    }

    // Validate required identifiers are present and within length caps.
    // Reject control characters in identifiers to avoid corrupting the JSONL
    // line format or downstream routing keys.
    let is_clean_id = |s: &str| s.chars().all(|c| !c.is_control());
    if chat_id.is_empty() || chat_id.len() > MAX_CHAT_ID_LEN || !is_clean_id(chat_id) {
        return Err(pyo3::exceptions::PyValueError::new_err(format!(
            "chat_id must be non-empty, <= {MAX_CHAT_ID_LEN} bytes, no control chars"
        )));
    }
    if sender.is_empty() || sender.len() > MAX_SENDER_LEN || !is_clean_id(sender) {
        return Err(pyo3::exceptions::PyValueError::new_err(format!(
            "sender must be non-empty, <= {MAX_SENDER_LEN} bytes, no control chars"
        )));
    }
    // `text` may be empty (e.g. sticker/attachment-only messages) but is capped.
    if text.len() > MAX_TEXT_LEN {
        return Err(pyo3::exceptions::PyValueError::new_err(format!(
            "text exceeds maximum length ({MAX_TEXT_LEN} bytes)"
        )));
    }

    let msg = serde_json::json!({
        "type": "incoming_message",
        "channel": channel,
        "chat_id": chat_id,
        "sender": sender,
        "text": text,
        "timestamp": chrono::Utc::now().to_rfc3339(),
    });

    write_to_queue(&msg)
        .map_err(|e| pyo3::exceptions::PyIOError::new_err(format!("Failed to write to bus queue: {e}")))?;

    Ok(())
}

/// The Python module implemented in Rust.
#[pymodule]
fn _native(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(version, m)?)?;
    m.add_function(wrap_pyfunction!(send_message, m)?)?;
    m.add_function(wrap_pyfunction!(send_to_bus, m)?)?;
    Ok(())
}
