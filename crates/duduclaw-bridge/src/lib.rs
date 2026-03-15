use pyo3::prelude::*;

/// Returns the version of the native bridge.
#[pyfunction]
fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Placeholder for sending a message through the Rust core.
#[pyfunction]
fn send_message(agent_id: &str, payload: &str) -> PyResult<String> {
    // TODO: implement in Phase 1
    Ok(format!(
        "TODO: send message to agent '{}' with payload '{}'",
        agent_id, payload
    ))
}

/// The Python module implemented in Rust.
#[pymodule]
fn _native(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(version, m)?)?;
    m.add_function(wrap_pyfunction!(send_message, m)?)?;
    Ok(())
}
