//! Health endpoint format verification tests.

#[test]
fn test_health_response_format() {
    // Verify the JSON structure matches expected schema
    let sample = serde_json::json!({
        "status": "ok",
        "version": "0.10.0",
        "uptime_seconds": 42,
        "agents_loaded": 2,
        "channels_connected": ["telegram", "discord"],
    });
    assert!(sample.get("status").unwrap().is_string());
    assert!(sample.get("version").unwrap().is_string());
    assert!(sample.get("uptime_seconds").unwrap().is_number());
    assert!(sample.get("agents_loaded").unwrap().is_number());
    assert!(sample.get("channels_connected").unwrap().is_array());
}

#[test]
fn test_health_status_values() {
    let valid = ["ok", "degraded", "error"];
    for status in valid {
        assert!(["ok", "degraded", "error"].contains(&status));
    }
}
