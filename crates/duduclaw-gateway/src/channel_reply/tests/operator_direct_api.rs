use super::*;

#[cfg(test)]
mod operator_direct_api_tool_loop_tests {
    use super::build_operator_tool_chat_request;
    use duduclaw_core::types::CapabilitiesConfig;
    use duduclaw_llm::{CacheHint, ContentPart, Role, ToolDef};

    fn dummy_tool(name: &str) -> ToolDef {
        ToolDef {
            name: name.to_string(),
            description: "test tool".to_string(),
            input_schema: serde_json::json!({"type": "object"}),
        }
    }

    // ── build_operator_tool_chat_request (pure, no I/O) ─────────────────

    #[test]
    fn request_carries_model_user_message_and_tools() {
        let req = build_operator_tool_chat_request(
            "claude-sonnet-5",
            "you are an operator",
            "list files",
            vec![dummy_tool("os_list_dir")],
        );
        assert_eq!(req.model, "claude-sonnet-5");
        assert_eq!(req.tools.len(), 1);
        assert_eq!(req.tools[0].name, "os_list_dir");
        assert_eq!(req.messages.len(), 1);
        assert_eq!(req.messages[0].role, Role::User);
        assert_eq!(
            req.messages[0].parts,
            vec![ContentPart::Text("list files".to_string())]
        );
    }

    #[test]
    fn system_prompt_without_marker_becomes_one_cached_block() {
        let req =
            build_operator_tool_chat_request("m", "single static system prompt", "hi", Vec::new());
        assert_eq!(req.system.len(), 1);
        // `split_system_segments` normalizes trailing whitespace per line via
        // `normalize_system_prompt`, appending one trailing `\n` for a
        // single-line input — the same normalization the tools-less
        // `direct_api.rs` path already applies (see its own test suite).
        assert_eq!(req.system[0].text, "single static system prompt\n");
        // `split_system_segments` cache-marks every segment it emits — same
        // "system_and_3" cache strategy as the tools-less `direct_api.rs`
        // path this function mirrors.
        assert_eq!(req.system[0].cache, CacheHint::Explicit);
    }

    #[test]
    fn system_prompt_with_cache_split_marker_becomes_multiple_blocks() {
        let system = format!(
            "static soul{}semi-stable wiki",
            duduclaw_llm::CACHE_SPLIT_MARKER
        );
        let req = build_operator_tool_chat_request("m", &system, "hi", Vec::new());
        assert_eq!(req.system.len(), 2);
        // Trailing `\n` per segment — see the normalization note above.
        assert_eq!(req.system[0].text, "static soul\n");
        assert_eq!(req.system[1].text, "semi-stable wiki\n");
    }

    #[test]
    fn empty_tools_produces_empty_tool_list_on_the_request() {
        // Exercised only via the `system_operator` caller — this asserts the
        // request-building piece itself has no hidden default that would
        // re-populate `req.tools` (that would defeat the fail-closed
        // capability filter applied by the caller before this is invoked).
        let req = build_operator_tool_chat_request("m", "sys", "hi", Vec::new());
        assert!(req.tools.is_empty());
    }

    // ── non-operator gate parity (matches the call site's inline check) ─

    #[test]
    fn default_capabilities_are_not_system_operator() {
        // The Direct-API fallback's `is_operator` gate is a plain
        // `capabilities.map(|c| c.system_operator).unwrap_or(false)` inline
        // at the call site (matching the same pattern used by the O-4 /
        // Task-C / R1 gates elsewhere in this file) — this pins the default
        // so every agent without an explicit `system_operator = true` never
        // reaches `try_operator_direct_api_tool_loop`, keeping its
        // Direct-API path byte-identical to before P34.
        let caps = CapabilitiesConfig::default();
        assert!(!caps.system_operator);
    }

    #[test]
    fn explicit_system_operator_capability_is_true() {
        let caps = CapabilitiesConfig {
            system_operator: true,
            ..Default::default()
        };
        assert!(caps.system_operator);
    }
}

