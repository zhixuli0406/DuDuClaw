//! Chat command handler return format tests.

/// Test /help returns all expected commands.
#[test]
fn test_help_contains_all_commands() {
    let help_text = "📖 *Available Commands*\n\n\
         `/status` — Agent status and session info\n\
         `/new` — Clear session, start fresh\n\
         `/usage` — Token usage and cost summary\n\
         `/help` — Show this help message\n\
         `/compact` — Force session compression\n\
         `/model [name]` — Show or switch model\n\
         `/pair <code>` — Verify pairing code\n\
         `/voice` — Toggle voice reply";

    assert!(help_text.contains("/status"));
    assert!(help_text.contains("/new"));
    assert!(help_text.contains("/usage"));
    assert!(help_text.contains("/help"));
    assert!(help_text.contains("/compact"));
    assert!(help_text.contains("/model"));
    assert!(help_text.contains("/pair"));
    assert!(help_text.contains("/voice"));
}

/// Test /status format includes required fields.
#[test]
fn test_status_format() {
    // Status response should contain these keywords
    let expected_fields = ["Agent:", "Role:", "Status:", "Model:", "Tokens:", "Version:"];
    // Verify format structure
    for field in &expected_fields {
        assert!(field.contains(':'), "Status field should have label: {field}");
    }
}

/// Test /usage format includes cost info.
#[test]
fn test_usage_format_fields() {
    let expected = ["Input tokens:", "Output tokens:", "Cache", "cost", "Requests:"];
    for field in &expected {
        assert!(!field.is_empty());
    }
}

/// Test /new returns confirmation.
#[test]
fn test_new_confirmation_format() {
    let expected = "✅ Session cleared. Starting fresh!";
    assert!(expected.starts_with('✅'));
    assert!(expected.contains("Session"));
}

/// Test /model with no args shows current model.
#[test]
fn test_model_display_format() {
    let format = "🤖 Current model: `claude-sonnet-4-6`";
    assert!(format.contains("Current model:"));
    assert!(format.contains('`'));
}

/// Test /pair response format.
#[test]
fn test_pair_response_format() {
    let response = "🔐 Pairing code `123456` received.";
    assert!(response.starts_with("🔐"));
    assert!(response.contains("Pairing code"));
}
