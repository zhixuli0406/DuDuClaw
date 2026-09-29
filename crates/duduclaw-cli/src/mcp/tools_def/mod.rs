//! The MCP tool registration face — what `tools/list` advertises.
//!
//! ## Why this is split by family (O7, 2026-09-29)
//!
//! This used to be one 4,252-line `const TOOLS: &[ToolDef]` literal. Every
//! entry here is a **fixed per-spawn prompt cost**: the CLI reads `tools/list`
//! once per session and the whole payload lands in the model's context before
//! the first user token, so the table is both a registry and a budget. Splitting
//! it by tool family keeps each file reviewable and makes the budget visible
//! per area instead of buried in a single wall of text.
//!
//! The groups are **contiguous slices of the original registration order** —
//! `tools/list` order is unchanged from the single-literal version, which keeps
//! the golden byte budget in `mcp::tools_list_budget_tests` comparable across
//! the split.
//!
//! ## Description budget
//!
//! A tool `description` is capped at 200 bytes and a `ParamDef` description at
//! 200 bytes, enforced by `mcp::tools_list_budget_tests`. Long prose — worked
//! examples, full JSON schemas, rationale — belongs in
//! `docs/guides/mcp-tools.md` (or the tool's own spec page, e.g.
//! `docs/spec/task-packet.md`), referenced from the description.
//!
//! ## Adding a tool
//!
//! 1. Add the `ToolDef` to the matching family file (create a new one and
//!    register it in [`TOOL_GROUPS`] if no family fits).
//! 2. Add the enforced scope arm in `mcp_auth::tool_requires_scope`.
//! 3. Add the catalog row in `duduclaw_core::tool_catalog` — the drift tests
//!    (`tool_catalog_covers_all_advertised_tools`,
//!    `test_catalog_scopes_match_tool_requires_scope`) fail otherwise.
//! 4. If the tool is gated by a per-agent capability, add it to the matching
//!    list in [`super::tools_list`] so *discoverable ⇔ callable* holds.

use super::{ToolDef, Value};

mod agents;
mod channels;
mod data;
mod exec;
mod fork;
mod google;
mod inference;
mod integrations;
mod memory;
mod odoo;
mod ops;
mod os;
mod recording;
mod skills;
mod tasks;
mod wiki;

/// Every tool family, in registration order. The concatenation of these slices
/// is exactly the old single `TOOLS` literal.
///
/// A `const` slice-of-slices rather than a `LazyLock<Vec<_>>` on purpose: the
/// table is static data and must not cost a heap allocation or a one-time lock
/// on the `tools/list` path.
pub(crate) const TOOL_GROUPS: &[&[ToolDef]] = &[
    channels::TOOLS,
    memory::TOOLS,
    agents::TOOLS,
    skills::TOOLS,
    ops::TOOLS,
    inference::TOOLS,
    odoo::TOOLS,
    google::TOOLS,
    integrations::TOOLS,
    wiki::TOOLS,
    exec::TOOLS,
    tasks::TOOLS,
    fork::TOOLS,
    data::TOOLS,
    os::TOOLS,
    recording::TOOLS,
];

/// Every advertised tool, in registration order.
///
/// Replaces the old `TOOLS.iter()`: Rust cannot concatenate `const` slices, so
/// the flattening lives here instead of at each call site.
pub(crate) fn tools() -> impl Iterator<Item = &'static ToolDef> + Clone {
    TOOL_GROUPS.iter().copied().flatten()
}

// ── External tool whitelist (W19-P0 BUG-QA-001) ─────────────
/// Tools visible to external MCP clients (`principal.is_external = true`).
/// Exactly 7 tools are exposed; all others are hidden to reduce attack surface.
pub(crate) const EXTERNAL_TOOLS_WHITELIST: &[&str] = &[
    "memory_search",
    "memory_store",
    "memory_read",
    "wiki_read",
    "wiki_write",
    "wiki_search",
    "send_message",
];

// ── JSON-RPC helpers ─────────────────────────────────────────

pub(crate) fn build_tool_schema(tool: &ToolDef) -> Value {
    let mut properties = serde_json::Map::new();
    let mut required = Vec::new();

    for param in tool.params {
        properties.insert(
            param.name.to_string(),
            serde_json::json!({
                "type": "string",
                "description": param.description
            }),
        );
        if param.required {
            required.push(Value::String(param.name.to_string()));
        }
    }

    serde_json::json!({
        "name": tool.name,
        "description": tool.description,
        "inputSchema": {
            "type": "object",
            "properties": properties,
            "required": required
        }
    })
}

#[cfg(test)]
mod registration_tests {
    use super::{EXTERNAL_TOOLS_WHITELIST, TOOL_GROUPS, tools};

    #[test]
    fn user_code_profile_tool_registered() {
        let tool = tools()
            .find(|t| t.name == "user_code_profile")
            .expect("user_code_profile must be registered in TOOLS");
        assert!(tool.params.is_empty(), "user_code_profile takes no params");
        // Internal-only: must NOT be on the external whitelist.
        assert!(!EXTERNAL_TOOLS_WHITELIST.contains(&"user_code_profile"));
    }

    /// The split must not have duplicated or dropped an entry: a tool name is
    /// the dispatch key, so two rows with the same name would advertise one
    /// schema and dispatch the other.
    #[test]
    fn tool_names_are_unique_across_groups() {
        let mut seen = std::collections::HashSet::new();
        for t in tools() {
            assert!(
                seen.insert(t.name),
                "duplicate tool registration: {}",
                t.name
            );
        }
        assert_eq!(
            seen.len(),
            TOOL_GROUPS.iter().map(|g| g.len()).sum::<usize>(),
            "group lengths disagree with the de-duplicated name count"
        );
    }
}
