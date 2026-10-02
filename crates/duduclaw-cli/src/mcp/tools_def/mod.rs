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

// ── Parameter types ──────────────────────────────────────────

/// JSON Schema types of the parameters that are not strings, as
/// `(tool, param, type)`. Every other parameter renders as `"string"`.
///
/// An overlay rather than a field on `ParamDef`: the table holds ~530
/// `ParamDef` literals that are all strings, and a field would have to be
/// spelled out on every one of them. `param_types_name_real_params` keeps
/// each row pointing at a parameter that exists.
pub(crate) const PARAM_TYPES: &[(&str, &str, &str)] = &[
    ("computer_click", "x", "integer"),
    ("computer_click", "y", "integer"),
    ("computer_click", "double", "boolean"),
    ("computer_scroll", "x", "integer"),
    ("computer_scroll", "y", "integer"),
    ("computer_scroll", "amount", "integer"),
    ("computer_session_start", "width", "integer"),
    ("computer_session_start", "height", "integer"),
];

/// The JSON Schema type of one parameter (see [`PARAM_TYPES`]).
pub(crate) fn param_type(tool: &str, param: &str) -> &'static str {
    PARAM_TYPES
        .iter()
        .find(|(t, p, _)| *t == tool && *p == param)
        .map(|(_, _, ty)| *ty)
        .unwrap_or("string")
}

// ── JSON-RPC helpers ─────────────────────────────────────────

pub(crate) fn build_tool_schema(tool: &ToolDef) -> Value {
    let mut properties = serde_json::Map::new();
    let mut required = Vec::new();

    for param in tool.params {
        properties.insert(
            param.name.to_string(),
            serde_json::json!({
                "type": param_type(tool.name, param.name),
                "description": param.description
            }),
        );
        if param.required {
            required.push(Value::String(param.name.to_string()));
        }
    }

    if tool.name == "tasks_create" {
        properties.insert("kind".into(),serde_json::json!({"type":"string","enum":["task","goal","discovery"],"default":"task"}));
        properties.insert("discovery".into(),serde_json::json!({
            "type":"object","additionalProperties":false,
            "properties":{
                "approved_root_id":{"type":"string"},"evaluator":{"type":"string"},
                "runtime":{"type":"string"},"model":{"type":"string"},
                "branch_count":{"type":"integer","minimum":1},
                "refine_count":{"type":"integer","minimum":0},
                "max_parallelism":{"type":"integer","minimum":1},
                "direction":{"type":"string","enum":["max","min"],"default":"max"},
                "budget":{"type":"object","additionalProperties":false,"properties":{
                    "max_agent_calls":{"type":"integer","minimum":1},
                    "max_usd":{"type":"number","exclusiveMinimum":0},
                    "max_wall_secs":{"type":"integer","minimum":1},
                    "max_rounds":{"type":"integer","minimum":1}},
                    "required":["max_agent_calls","max_usd","max_wall_secs","max_rounds"]}
            },"required":["approved_root_id","evaluator","runtime","model","branch_count","refine_count","max_parallelism","budget"]
        }));
    }
    if tool.name == "discovery_list" {
        properties.insert("limit".into(),serde_json::json!({"type":"integer","minimum":1,"maximum":100,"default":20}));
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
    use super::{EXTERNAL_TOOLS_WHITELIST, PARAM_TYPES, TOOL_GROUPS, build_tool_schema, tools};

    #[test]
    fn param_types_name_real_params() {
        for (tool, param, ty) in PARAM_TYPES {
            let def = tools().find(|t| t.name == *tool).unwrap_or_else(|| panic!("{tool} not registered"));
            assert!(def.params.iter().any(|p| p.name == *param), "{tool}.{param} does not exist");
            assert!(matches!(*ty, "integer" | "boolean" | "number"), "{tool}.{param}: {ty}");
        }
    }

    #[test]
    fn computer_tool_schemas_declare_integer_and_boolean_params() {
        let schema = |name: &str| build_tool_schema(tools().find(|t| t.name == name).unwrap());
        let click = schema("computer_click");
        let props = &click["inputSchema"]["properties"];
        assert_eq!(props["x"]["type"], "integer");
        assert_eq!(props["y"]["type"], "integer");
        assert_eq!(props["double"]["type"], "boolean");
        assert_eq!(props["button"]["type"], "string");
        assert_eq!(click["inputSchema"]["required"], serde_json::json!(["x", "y"]));
        let scroll = schema("computer_scroll");
        assert_eq!(scroll["inputSchema"]["properties"]["amount"]["type"], "integer");
        let start = schema("computer_session_start");
        assert_eq!(start["inputSchema"]["properties"]["width"]["type"], "integer");
        assert_eq!(start["inputSchema"]["required"], serde_json::json!([]));
        // Other tools are untouched.
        let other = schema("memory_search");
        for (_, p) in other["inputSchema"]["properties"].as_object().unwrap() {
            assert_eq!(p["type"], "string");
        }
    }

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
