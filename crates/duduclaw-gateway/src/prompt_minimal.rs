//! Minimal system prompt builder (#11 Active Retrieval, 2026-05-12).
//!
//! Anthropic Skills-style assembly: emit only the stable core
//! (SOUL/IDENTITY/CONTRACT) plus an index of MCP tools. The agent then
//! pulls wiki / skill content on demand via tool calls instead of having
//! it pre-injected. Designed for agents that hit the 200 K cliff because
//! conversation history plus inlined wiki overflow the cache window.
//!
//! Compared with the `Full` builder in `claude_runner` / `channel_reply`,
//! the `Minimal` path:
//!
//! - Truncates SOUL.md to `minimal_core_kb` × 1024 bytes (first slice —
//!   typically persona + principles, not appendices)
//! - Drops wiki injection entirely (agents discover via `wiki_search`)
//! - Drops skill bodies (agents discover via `skill_list` / `skill_load`)
//! - Drops team roster verbatim (agents query via `list_agents`)
//! - Keeps IDENTITY.md, CONTRACT, sender block, pinned task as-is
//!
//! See [`commercial/docs/TODO-runtime-health-fixes-202605.md#11`](
//! ../../../commercial/docs/TODO-runtime-health-fixes-202605.md
//! ) for the rationale and ROI estimate (75 % cliff reduction).

use duduclaw_agent::LoadedAgent;

/// Hard ceiling for IDENTITY.md when truncating in minimal mode.
///
/// IDENTITY.md is usually short (a few sentences naming the agent +
/// brand voice). 2 KB is generous; longer files are typically misuse
/// of IDENTITY.md as a notes dump, which the minimal mode rejects on
/// purpose.
const IDENTITY_MAX_BYTES: usize = 2 * 1024;

/// Build the minimal system prompt for an agent.
///
/// Thin LoadedAgent → pure-args adapter. The real logic lives in
/// [`build_minimal_inner`] so it can be tested without constructing a
/// LoadedAgent — `AgentConfig` has too many non-defaultable sub-configs
/// to make fixtures ergonomic, and the rendering policy doesn't actually
/// depend on most of them.
pub fn build_minimal_system_prompt(
    agent: &LoadedAgent,
    sender_block: &str,
    pinned_instructions: &str,
) -> String {
    let contract_prompt = duduclaw_agent::contract::contract_to_prompt(&agent.contract);
    build_minimal_inner(MinimalInput {
        agent_name: &agent.config.agent.name,
        soul: agent.soul.as_deref(),
        identity: agent.identity.as_deref(),
        contract: &contract_prompt,
        sender_block,
        pinned_instructions,
        minimal_core_kb: agent.config.prompt.minimal_core_kb,
    })
}

/// Pure-args shape for the minimal renderer. Lets tests pin behaviour
/// without touching `LoadedAgent` or filesystem.
pub(crate) struct MinimalInput<'a> {
    pub agent_name: &'a str,
    pub soul: Option<&'a str>,
    pub identity: Option<&'a str>,
    pub contract: &'a str,
    pub sender_block: &'a str,
    pub pinned_instructions: &'a str,
    pub minimal_core_kb: u32,
}

pub(crate) fn build_minimal_inner(input: MinimalInput<'_>) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut audit: Vec<crate::prompt_audit::PromptSection> = Vec::new();

    // 1. SOUL core (truncated to minimal_core_kb)
    if let Some(soul) = input.soul {
        let max_bytes = (input.minimal_core_kb as usize) * 1024;
        let trimmed = truncate_to_byte_budget(soul, max_bytes);
        let s = if trimmed.len() < soul.len() {
            format!(
                "{}\n\n[truncated for minimal mode; full SOUL available via shared_wiki_read]",
                trimmed
            )
        } else {
            trimmed.to_string()
        };
        audit.push(crate::prompt_audit::PromptSection::new("soul_core", &s));
        parts.push(s);
    }

    // 2. IDENTITY.md — short by convention; truncate at IDENTITY_MAX_BYTES
    if let Some(identity) = input.identity {
        let trimmed = truncate_to_byte_budget(identity, IDENTITY_MAX_BYTES);
        audit.push(crate::prompt_audit::PromptSection::new("identity", trimmed));
        parts.push(trimmed.to_string());
    }

    // 3. CONTRACT — keep verbatim, it's the safety guard
    if !input.contract.is_empty() {
        audit.push(crate::prompt_audit::PromptSection::new(
            "contract",
            input.contract,
        ));
        parts.push(input.contract.to_string());
    }

    // 4. Sender block (RFC-21) — small XML island, always include if present
    if !input.sender_block.is_empty() {
        audit.push(crate::prompt_audit::PromptSection::new(
            "sender",
            input.sender_block,
        ));
        parts.push(input.sender_block.to_string());
    }

    // 5. MCP tool index — the active-retrieval contract: agent knows what's
    //    available, fetches on demand. Keep the list short and stable;
    //    cache-friendly because it's the same string for every call.
    parts.push(mcp_tool_index().to_string());

    // 6. Pinned task at the bottom (Anthropic best practice — high-attention tail)
    if !input.pinned_instructions.is_empty() {
        let s = format!(
            "## Pinned Task Instructions\n\
             The user's core task requirements (ALWAYS follow these throughout the conversation):\n\
             {pinned}",
            pinned = input.pinned_instructions
        );
        audit.push(crate::prompt_audit::PromptSection::new("pinned", &s));
        parts.push(s);
    }

    crate::prompt_audit::maybe_log_breakdown(
        input.agent_name,
        "prompt_minimal",
        &audit,
        crate::prompt_audit::DEFAULT_EMIT_THRESHOLD_BYTES,
    );

    parts.join("\n\n---\n\n")
}

/// Slice the first `max_bytes` of `s` along a UTF-8 char boundary so
/// CJK strings never get split mid-character. Returns a borrowed slice
/// that's safe to format.
///
/// Extracted as a pure helper because we need the exact same truncation
/// rule across SOUL/IDENTITY and any future minimal section. Pinning the
/// boundary semantics in tests prevents silent panics if someone swaps
/// the slice with raw byte indexing later.
pub(crate) fn truncate_to_byte_budget(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    // Walk back from max_bytes to the nearest char boundary so we never
    // slice mid-codepoint. `is_char_boundary(0)` is always true so this
    // terminates.
    let mut end = max_bytes;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// The fixed MCP tool index injected in place of inlined wiki/skill bodies.
///
/// This is the contract that makes minimal mode work: the agent sees what's
/// available and how to ask for it, instead of having everything dumped in.
/// Kept as a const so the string is identical across every call → maximum
/// cache stability.
fn mcp_tool_index() -> &'static str {
    "## Available MCP Tools (#11 Active Retrieval)\n\
     Wiki / skill / memory content is no longer inlined here — fetch it on \
     demand:\n\
     - `wiki_search(query)` — full-text + tag search across the shared wiki\n\
     - `wiki_read(path)` — load a specific wiki page by path\n\
     - `shared_wiki_search` / `shared_wiki_read` — same for cross-agent wiki\n\
     - `skill_list()` — enumerate available skills (name + description only)\n\
     - `skill_load(name)` — load full skill body when you decide to use it\n\
     - `memory_search(query)` — semantic search over your memory engine\n\
     - `list_agents()` — current sub-agent roster (display name, role, status)\n\
     \n\
     Use these proactively. Don't ask the user what's in the wiki — search it. \
     Don't hallucinate sub-agent names — call `list_agents`."
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input<'a>(
        soul: Option<&'a str>,
        identity: Option<&'a str>,
        sender: &'a str,
        pinned: &'a str,
        core_kb: u32,
    ) -> MinimalInput<'a> {
        MinimalInput {
            agent_name: "test",
            soul,
            identity,
            contract: "",
            sender_block: sender,
            pinned_instructions: pinned,
            minimal_core_kb: core_kb,
        }
    }

    #[test]
    fn truncate_returns_full_when_under_budget() {
        let s = "hello world";
        assert_eq!(truncate_to_byte_budget(s, 100), "hello world");
    }

    #[test]
    fn truncate_respects_max_bytes() {
        let s = "abcdefghij"; // 10 bytes
        assert_eq!(truncate_to_byte_budget(s, 5), "abcde");
    }

    #[test]
    fn truncate_walks_back_to_char_boundary_for_cjk() {
        // 你好 = 6 bytes (each char is 3 bytes UTF-8). Cutting at 4 would slice mid-codepoint.
        // The helper must walk back to byte index 3 ("你") instead.
        let s = "你好";
        assert_eq!(truncate_to_byte_budget(s, 4), "你");
        // Cutting at exactly 3 is a valid boundary.
        assert_eq!(truncate_to_byte_budget(s, 3), "你");
        // 0 is always valid.
        assert_eq!(truncate_to_byte_budget(s, 0), "");
    }

    #[test]
    fn minimal_prompt_truncates_soul_to_kb_budget() {
        // 10 KB SOUL, 2 KB budget → first ~2 KB chars + truncation footer.
        let big_soul = "abc\n".repeat(2500); // 10_000 bytes
        let prompt = build_minimal_inner(input(Some(&big_soul), None, "", "", 2));
        assert!(
            prompt.contains("[truncated for minimal mode"),
            "expected truncation marker; got:\n{prompt}"
        );
        assert!(
            prompt.len() < big_soul.len(),
            "minimal prompt ({} bytes) must be smaller than original SOUL ({} bytes)",
            prompt.len(),
            big_soul.len()
        );
    }

    #[test]
    fn minimal_prompt_skips_truncation_marker_when_soul_under_budget() {
        let prompt = build_minimal_inner(input(Some("Be friendly."), None, "", "", 5));
        assert!(!prompt.contains("[truncated"));
        assert!(prompt.contains("Be friendly."));
    }

    #[test]
    fn minimal_prompt_always_includes_mcp_index() {
        let prompt = build_minimal_inner(input(Some("soul"), None, "", "", 5));
        assert!(prompt.contains("Available MCP Tools"));
        assert!(prompt.contains("wiki_search"));
        assert!(prompt.contains("skill_list"));
        assert!(prompt.contains("list_agents"));
    }

    #[test]
    fn minimal_prompt_omits_blank_optional_sections() {
        let prompt = build_minimal_inner(input(Some("soul"), None, "", "", 5));
        assert!(!prompt.contains("<sender>"));
        assert!(!prompt.contains("Pinned Task Instructions"));
    }

    #[test]
    fn minimal_prompt_includes_pinned_at_tail() {
        let prompt = build_minimal_inner(input(
            Some("soul"),
            None,
            "",
            "Use polite tone.",
            5,
        ));
        let mcp_pos = prompt.find("Available MCP Tools").unwrap();
        let pin_pos = prompt.find("Pinned Task Instructions").unwrap();
        assert!(
            pin_pos > mcp_pos,
            "pinned instructions must appear after the MCP index (high-attention tail)"
        );
        assert!(prompt.contains("Use polite tone."));
    }

    #[test]
    fn minimal_prompt_size_well_under_full_for_knowledge_rich_agent() {
        // Acceptance gate: a 50 KB SOUL stuffed full of details should
        // still produce a minimal prompt under ~10 KB when minimal_core_kb=5.
        let fat_soul = "x".repeat(50_000);
        let prompt = build_minimal_inner(input(Some(&fat_soul), None, "", "", 5));
        assert!(
            prompt.len() < 10_000,
            "minimal prompt size {} should be < 10KB even with 50KB SOUL",
            prompt.len()
        );
    }

    #[test]
    fn minimal_prompt_includes_sender_block_when_present() {
        let sender = "<sender>resolved-person</sender>";
        let prompt = build_minimal_inner(input(Some("soul"), None, sender, "", 5));
        assert!(prompt.contains("<sender>resolved-person</sender>"));
    }

    #[test]
    fn minimal_prompt_includes_identity_truncated_to_2kb() {
        // 5 KB identity → trimmed at 2 KB (IDENTITY_MAX_BYTES).
        let big_id = "id\n".repeat(2000); // ~6 KB
        let prompt = build_minimal_inner(input(Some("soul"), Some(&big_id), "", "", 5));
        // Identity section length is bounded.
        // The whole prompt minus the SOUL/MCP-index parts should be < 2.5KB.
        let mcp_pos = prompt.find("Available MCP Tools").unwrap();
        let identity_slice = &prompt[..mcp_pos];
        assert!(
            identity_slice.len() < 2 * 1024 + 200, // soul "soul" + sep + identity 2K + sep
            "identity section ({} bytes) must respect IDENTITY_MAX_BYTES",
            identity_slice.len()
        );
    }
}
