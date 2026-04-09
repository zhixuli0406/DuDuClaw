use super::guidelines::*;
use super::tool_classifier::*;

// ── Helper ──────────────────────────────────────────────────────────

fn make_meta(name: &str, text: &str, turn_age: usize, referenced: bool) -> ToolCallMeta {
    ToolCallMeta {
        tool_name: name.to_string(),
        result_text: text.to_string(),
        turn_age,
        referenced_by_user: referenced,
        token_estimate: estimate_tokens(text),
    }
}

// ── ToolResultClassifier tests ──────────────────────────────────────

#[test]
fn recent_result_is_full() {
    let c = ToolResultClassifier::new();
    let meta = make_meta("file_read", "contents of file", 1, false);
    assert_eq!(c.classify(&meta), ToolResultFidelity::Full);
}

#[test]
fn recent_result_at_boundary_is_full() {
    let c = ToolResultClassifier::new(); // recency_window = 2
    let meta = make_meta("file_read", "contents", 2, false);
    assert_eq!(c.classify(&meta), ToolResultFidelity::Full);
}

#[test]
fn old_result_is_placeholder() {
    let c = ToolResultClassifier::new();
    let meta = make_meta("file_read", "contents of file", 6, false);
    assert_eq!(c.classify(&meta), ToolResultFidelity::Placeholder);
}

#[test]
fn user_referenced_always_full_regardless_of_age() {
    let c = ToolResultClassifier::new();
    let meta = make_meta("file_read", "old content", 100, true);
    assert_eq!(c.classify(&meta), ToolResultFidelity::Full);
}

#[test]
fn health_check_tool_is_discarded() {
    let c = ToolResultClassifier::new();
    let meta = make_meta("health_check", "all systems nominal", 3, false);
    assert_eq!(c.classify(&meta), ToolResultFidelity::Discard);
}

#[test]
fn ping_tool_is_discarded() {
    let c = ToolResultClassifier::new();
    let meta = make_meta("ping", "pong", 3, false);
    assert_eq!(c.classify(&meta), ToolResultFidelity::Discard);
}

#[test]
fn short_ok_result_is_discarded() {
    let c = ToolResultClassifier::new();
    let meta = make_meta("some_tool", "ok", 4, false);
    assert_eq!(c.classify(&meta), ToolResultFidelity::Discard);
}

#[test]
fn short_success_result_is_discarded() {
    let c = ToolResultClassifier::new();
    let meta = make_meta("some_tool", "success", 4, false);
    assert_eq!(c.classify(&meta), ToolResultFidelity::Discard);
}

#[test]
fn json_health_response_is_discarded() {
    let c = ToolResultClassifier::new();
    let meta = make_meta(
        "api_call",
        r#"{ "status": "ok", "uptime": 12345 }"#,
        4,
        false,
    );
    assert_eq!(c.classify(&meta), ToolResultFidelity::Discard);
}

#[test]
fn compressed_window_result() {
    let c = ToolResultClassifier::new(); // compressed_window = 5
    let meta = make_meta("file_read", "some meaningful content here", 3, false);
    assert_eq!(c.classify(&meta), ToolResultFidelity::Compressed);
}

#[test]
fn apply_fidelity_full_returns_original() {
    let meta = make_meta("tool", "hello world", 1, false);
    let result = ToolResultClassifier::apply_fidelity(ToolResultFidelity::Full, &meta);
    assert_eq!(result, "hello world");
}

#[test]
fn apply_fidelity_compressed_truncates_long_text() {
    let long_text = "a".repeat(500);
    let meta = make_meta("tool", &long_text, 3, false);
    let result = ToolResultClassifier::apply_fidelity(ToolResultFidelity::Compressed, &meta);
    // Should be head(200) + "..." + tail(100) = 303 chars
    assert!(result.contains("..."));
    assert_eq!(result.len(), 303);
}

#[test]
fn apply_fidelity_compressed_keeps_short_text() {
    let short_text = "a".repeat(250);
    let meta = make_meta("tool", &short_text, 3, false);
    let result = ToolResultClassifier::apply_fidelity(ToolResultFidelity::Compressed, &meta);
    assert_eq!(result, short_text);
}

#[test]
fn apply_fidelity_placeholder_returns_stub() {
    let meta = make_meta("file_read", "some content", 10, false);
    let result = ToolResultClassifier::apply_fidelity(ToolResultFidelity::Placeholder, &meta);
    assert_eq!(
        result,
        "[tool: file_read, called at turn 10, result archived]"
    );
}

#[test]
fn apply_fidelity_discard_returns_empty() {
    let meta = make_meta("ping", "pong", 5, false);
    let result = ToolResultClassifier::apply_fidelity(ToolResultFidelity::Discard, &meta);
    assert!(result.is_empty());
}

#[test]
fn batch_processing_counts_correct() {
    let c = ToolResultClassifier::new();
    let mut batch = vec![
        make_meta("file_read", "recent content", 1, false),      // Full
        make_meta("api_call", "medium-age data here", 3, false),  // Compressed
        make_meta("old_tool", "ancient data", 10, false),         // Placeholder
        make_meta("health_check", "ok", 3, false),                // Discard
    ];

    let result = c.process_batch(&mut batch);
    assert_eq!(result.full_count, 1);
    assert_eq!(result.compressed_count, 1);
    assert_eq!(result.placeholder_count, 1);
    assert_eq!(result.discarded_count, 1);
}

#[test]
fn batch_processing_saves_tokens() {
    let c = ToolResultClassifier::new();
    let long_text = "x".repeat(1000);
    let mut batch = vec![
        make_meta("old_tool", &long_text, 10, false), // Placeholder
        make_meta("health_check", "ok", 3, false),    // Discard
    ];

    let result = c.process_batch(&mut batch);
    assert!(result.tokens_saved > 0);
    assert!(result.compressed_tokens < result.original_tokens);
}

// ── GuidelineManager tests ──────────────────────────────────────────

#[test]
fn new_failure_creates_compressed_guideline() {
    let mut mgr = GuidelineManager::new();
    mgr.record_failure("file_read", "agent needed file content");
    let g = mgr.get("file_read").expect("guideline should exist");
    assert_eq!(g.min_fidelity, ToolResultFidelity::Compressed);
    assert!(g.created_from_failure);
}

#[test]
fn repeated_failure_elevates_to_full() {
    let mut mgr = GuidelineManager::new();
    mgr.record_failure("file_read", "first failure");
    assert_eq!(
        mgr.get("file_read").unwrap().min_fidelity,
        ToolResultFidelity::Compressed
    );
    mgr.record_failure("file_read", "second failure");
    assert_eq!(
        mgr.get("file_read").unwrap().min_fidelity,
        ToolResultFidelity::Full
    );
}

#[test]
fn enforce_minimum_respects_guideline() {
    let mut mgr = GuidelineManager::new();
    mgr.record_failure("file_read", "needed content");
    // Guideline is Compressed; proposing Placeholder should elevate to Compressed.
    let result = mgr.enforce_minimum("file_read", ToolResultFidelity::Placeholder);
    assert_eq!(result, ToolResultFidelity::Compressed);
}

#[test]
fn enforce_minimum_returns_proposed_if_higher() {
    let mut mgr = GuidelineManager::new();
    mgr.record_failure("file_read", "needed content");
    // Guideline is Compressed; proposing Full should stay Full.
    let result = mgr.enforce_minimum("file_read", ToolResultFidelity::Full);
    assert_eq!(result, ToolResultFidelity::Full);
}

#[test]
fn enforce_minimum_no_guideline_returns_proposed() {
    let mgr = GuidelineManager::new();
    let result = mgr.enforce_minimum("unknown_tool", ToolResultFidelity::Placeholder);
    assert_eq!(result, ToolResultFidelity::Placeholder);
}

#[test]
fn serialization_roundtrip() {
    let mut mgr = GuidelineManager::new();
    mgr.record_failure("tool_a", "reason a");
    mgr.record_failure("tool_b", "reason b");

    let json = mgr.to_json().expect("serialization should succeed");
    let mgr2 =
        GuidelineManager::load_from_json(&json).expect("deserialization should succeed");

    assert!(mgr2.get("tool_a").is_some());
    assert!(mgr2.get("tool_b").is_some());
    assert_eq!(
        mgr2.get("tool_a").unwrap().min_fidelity,
        ToolResultFidelity::Compressed
    );
}

#[test]
fn custom_windows_affect_classification() {
    let c = ToolResultClassifier::with_windows(1, 3);
    // turn_age=2 is outside recency(1) but inside compressed(3)
    let meta = make_meta("tool", "some data", 2, false);
    assert_eq!(c.classify(&meta), ToolResultFidelity::Compressed);

    // turn_age=1 is inside recency(1)
    let meta2 = make_meta("tool", "some data", 1, false);
    assert_eq!(c.classify(&meta2), ToolResultFidelity::Full);

    // turn_age=4 is outside compressed(3) -> Placeholder
    let meta3 = make_meta("tool", "some data", 4, false);
    assert_eq!(c.classify(&meta3), ToolResultFidelity::Placeholder);
}
