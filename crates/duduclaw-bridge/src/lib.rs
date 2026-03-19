use pyo3::prelude::*;

/// Returns the version of the native bridge.
#[pyfunction]
fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Get the DuDuClaw home directory.
fn get_duduclaw_home() -> std::path::PathBuf {
    if let Ok(home) = std::env::var("DUDUCLAW_HOME") {
        return std::path::PathBuf::from(home);
    }
    let home = std::env::var("HOME").unwrap_or_default();
    std::path::PathBuf::from(home).join(".duduclaw")
}

/// Write a JSON line to the bus queue file for the Rust gateway to process.
fn write_to_queue(msg: &serde_json::Value) -> std::io::Result<()> {
    let queue_path = get_duduclaw_home().join("bus_queue.jsonl");
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&queue_path)?;
    use std::io::Write;
    writeln!(file, "{}", msg)
}

/// Send a message to a specific agent via the Rust core bus.
///
/// The message is written to `~/.duduclaw/bus_queue.jsonl` which the
/// running gateway polls to deliver cross-agent messages.
#[pyfunction]
fn send_message(agent_id: &str, payload: &str) -> PyResult<String> {
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
