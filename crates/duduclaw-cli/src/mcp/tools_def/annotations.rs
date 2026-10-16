//! MCP tool `annotations` (`readOnlyHint` / `destructiveHint` /
//! `idempotentHint` / `openWorldHint`) for every advertised tool.
//!
//! ## One source of truth
//!
//! The hints are derived from [`duduclaw_core::effect_of`], the same effect
//! table the dispatch gate and `action_rules` use, so a tool cannot be gated
//! as `modify` while advertising itself as read-only. Only three short
//! exact-name overlays refine what the effect class alone cannot say.
//!
//! ## Hints are never authorization
//!
//! A client may use `readOnlyHint` to skip its own confirmation prompt; every
//! DuDuClaw gate still runs on the call. That is why the overlays may only
//! ever *relax a client prompt for a tool that really is harmless*: a
//! `delete` / `purchase` / `admin` tool can never be listed as additive
//! (`annotation_overlays_never_soften_dangerous_classes` pins it).
//!
//! ## Not part of the prompt budget
//!
//! Claude Code and Codex hand the model a tool's name, description and input
//! schema; annotations stay client-side. The O7 budget test therefore
//! measures the listing without them (`tools_list_budget_tests`).

use duduclaw_core::{ToolAnnotations, ToolEffect};
use serde_json::Value;

/// Tools whose every effect stays inside DuDuClaw's own stores (memory, wiki,
/// working state, user profiles, the local repository scan). They get
/// `openWorldHint: false`; every other tool keeps the MCP default (`true`).
pub(crate) const CLOSED_WORLD_TOOLS: &[&str] = &[
    "code_map",
    "memory_alias_add",
    "memory_alias_list",
    "memory_fetch_batch",
    "memory_get_at",
    "memory_get_history",
    "memory_improve",
    "memory_read",
    "memory_search",
    "memory_store",
    "user_code_profile",
    "user_profile_get",
    "user_profile_record",
    "wiki_dedup",
    "wiki_export",
    "wiki_graph",
    "wiki_lint",
    "wiki_ls",
    "wiki_read",
    "wiki_rebuild_fts",
    "wiki_search",
    "wiki_share",
    "wiki_stats",
    "wiki_write",
    "working_state_get",
];

/// `modify`-class tools that only add or regenerate: they never overwrite or
/// remove anything a person wrote (`memory_store` inserts, a profile fact is
/// superseded with its history kept, the FTS index is derived data nobody
/// edits). They advertise `destructiveHint: false`. `memory_alias_add` is not
/// here: it upserts, so re-adding an alias re-points it. `wiki_export` is not
/// here either: it overwrites the previous `exports/<agent>-wiki.html` or the
/// files of the Obsidian vault in place, and a person may have edited that
/// vault since the last export.
pub(crate) const ADDITIVE_MODIFY_TOOLS: &[&str] = &[
    "memory_store",
    "user_profile_record",
    "wiki_rebuild_fts",
];

/// Non-read-only tools where repeating a call with the same arguments leaves
/// the same state as calling once.
pub(crate) const IDEMPOTENT_TOOLS: &[&str] =
    &["memory_alias_add", "wiki_export", "wiki_rebuild_fts"];

/// The `annotations` object for one tool, in the same
/// [`ToolAnnotations`] shape DuDuClaw reads from third-party servers.
pub(crate) fn tool_annotations(name: &str) -> Value {
    let effect = duduclaw_core::effect_of(name);
    let read_only = effect == ToolEffect::Read;
    let hints = ToolAnnotations {
        read_only: Some(read_only),
        // `destructiveHint` / `idempotentHint` are meaningless on a
        // read-only tool (MCP spec), so they are left out there.
        destructive: (!read_only).then(|| is_destructive(effect, name)),
        idempotent: (!read_only && IDEMPOTENT_TOOLS.contains(&name)).then_some(true),
        open_world: CLOSED_WORLD_TOOLS.contains(&name).then_some(false),
    };
    serde_json::to_value(hints).unwrap_or_else(|_| Value::Object(Default::default()))
}

/// `draft` / `send` add something new; `publish` and `modify` may replace
/// what is there (`modify` unless overlaid); `delete` / `purchase` / `admin`
/// are always destructive.
fn is_destructive(effect: ToolEffect, name: &str) -> bool {
    match effect {
        ToolEffect::Read | ToolEffect::Draft | ToolEffect::Send => false,
        // Publishing can replace what others already read (`wiki_share`
        // overwrites its earlier copy) and cannot be taken back.
        ToolEffect::Publish => true,
        ToolEffect::Modify => !ADDITIVE_MODIFY_TOOLS.contains(&name),
        ToolEffect::Delete | ToolEffect::Purchase | ToolEffect::Admin => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::tools_def::tools;

    #[test]
    fn annotation_overlays_name_real_tools() {
        for list in [CLOSED_WORLD_TOOLS, ADDITIVE_MODIFY_TOOLS, IDEMPOTENT_TOOLS] {
            for name in list {
                assert!(
                    tools().any(|t| t.name == *name),
                    "{name} is not a registered tool"
                );
            }
        }
    }

    /// The additive overlay may only refine `modify`; it can never make a
    /// delete, purchase or admin tool look harmless.
    #[test]
    fn annotation_overlays_never_soften_dangerous_classes() {
        for name in ADDITIVE_MODIFY_TOOLS {
            assert_eq!(duduclaw_core::effect_of(name), ToolEffect::Modify, "{name}");
        }
        for name in IDEMPOTENT_TOOLS {
            assert_ne!(
                duduclaw_core::effect_of(name),
                ToolEffect::Read,
                "{name}: hint is moot"
            );
        }
        for t in tools() {
            let effect = duduclaw_core::effect_of(t.name);
            if matches!(
                effect,
                ToolEffect::Delete | ToolEffect::Purchase | ToolEffect::Admin
            ) {
                assert_eq!(
                    tool_annotations(t.name)["destructiveHint"],
                    true,
                    "{}",
                    t.name
                );
            }
        }
    }

    /// `readOnlyHint` is true exactly for the tools the gate classifies `read`.
    #[test]
    fn read_only_hint_matches_the_effect_table() {
        for t in tools() {
            let read = duduclaw_core::effect_of(t.name) == ToolEffect::Read;
            assert_eq!(tool_annotations(t.name)["readOnlyHint"], read, "{}", t.name);
        }
    }

    #[test]
    fn standalone_tools_carry_expected_hints() {
        let a = tool_annotations("memory_search");
        assert_eq!(a["readOnlyHint"], true);
        assert_eq!(a["openWorldHint"], false);
        assert!(a.get("destructiveHint").is_none());

        let a = tool_annotations("memory_store");
        assert_eq!(a["readOnlyHint"], false);
        assert_eq!(a["destructiveHint"], false);

        // Appends to _log.md on every call: not idempotent.
        let a = tool_annotations("wiki_write");
        assert_eq!(a["destructiveHint"], true);
        assert!(a.get("idempotentHint").is_none());
        assert_eq!(tool_annotations("wiki_share")["destructiveHint"], true);

        // Overwrites the previous export (an Obsidian vault a person may
        // have edited): destructive, but repeating it gives the same files.
        let a = tool_annotations("wiki_export");
        assert_eq!(a["destructiveHint"], true);
        assert_eq!(a["idempotentHint"], true);

        // Rotates the internal MCP key when old: admin, not read-only.
        let a = tool_annotations("os_doctor_repair");
        assert_eq!(a["readOnlyHint"], false);
        assert_eq!(a["destructiveHint"], true);

        // A tool that reaches outside keeps the open-world default.
        assert!(
            tool_annotations("web_search")
                .get("openWorldHint")
                .is_none()
        );
        // Unknown names fail closed to admin.
        assert_eq!(tool_annotations("no_such_tool")["destructiveHint"], true);
    }
}
