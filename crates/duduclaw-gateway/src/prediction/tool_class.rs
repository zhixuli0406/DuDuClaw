//! WP-A1 — `ToolClass`: the only runtime-neutral tool-semantics unit.
//!
//! See `commercial/docs/design-task-forward-model-2026-08-06.md` §2.2. A
//! specific tool name is NOT comparable across runtimes — Claude's `Bash`,
//! codex's shell item, and gemini's `run_shell_command` are the same
//! *thing* under three different names. Prediction and diff therefore never
//! operate on tool names directly; they operate on `ToolClass`. New
//! runtimes/tools only need to extend [`ToolClass::classify`]'s mapping
//! table — no algorithm changes.
//!
//! ## Mapping precedence (deterministic, no unanchored substring checks —
//! project convention #2)
//!
//! 1. Strip the exact `mcp__duduclaw__` prefix if present (full literal
//!    match, not `contains`).
//! 2. Exact-match against the native/CLI built-in tool table (Claude Code's
//!    own tools: `Bash`, `Read`, `Write`, …).
//! 3. Exact-match against the duduclaw MCP tool table (~207 tool names as of
//!    2026-08-06, enumerated from `crates/duduclaw-cli/src/mcp.rs`'s
//!    `ToolDef` list).
//! 4. No match ⇒ [`ToolClass::Other`].
//!
//! R4 (design §10.1): the mapping table is maintenance debt — a new MCP
//! tool or runtime that isn't added here silently falls to `Other`. Rather
//! than let that go unnoticed, [`other_ratio_exceeds_threshold`] gives
//! callers a cheap way to emit a `tracing::warn!` when `Other` accounts for
//! an unhealthy share of a batch, so a mapping gap surfaces instead of
//! quietly degrading prediction quality.

use std::collections::BTreeSet;

use duduclaw_core::types::RuntimeType;
use serde::{Deserialize, Serialize};

/// Exact literal MCP tool-name prefix stripped before classification.
/// **Not** a `starts_with`-anywhere check — this is a single, fixed,
/// anchored-at-the-start prefix (project convention #2: no unanchored
/// substring routing decisions).
const MCP_DUDUCLAW_PREFIX: &str = "mcp__duduclaw__";

/// R4 mitigation: log-worthy `Other` ratio threshold (see design §10.1 R4).
pub const OTHER_RATIO_WARN_THRESHOLD: f64 = 0.20;

/// Tool class — the only runtime-neutral unit predictions/observations are
/// expressed in. See module docs for the mapping precedence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolClass {
    /// Read file / directory / wiki.
    Read,
    /// Write file / edit / create.
    Write,
    /// Search (grep / glob / memory retrieval / wiki search).
    Search,
    /// Execute (shell / script / build / test).
    Exec,
    /// External network (fetch / API / channel send).
    Net,
    /// Delegate (spawn_agent / send_to_agent / sub-agent).
    Delegate,
    /// Task board (tasks_* / activity_*).
    TaskBoard,
    /// Memory read/write (memory_*).
    Memory,
    /// Approval / capability request.
    Approval,
    /// Does not map to a known class.
    Other,
}

impl ToolClass {
    /// Deterministic classification. See module docs for precedence.
    ///
    /// `runtime` is currently only consulted implicitly (all runtimes share
    /// one native-tool exact-match table plus a handful of cross-runtime
    /// aliases for the "same thing, different name" cases the design calls
    /// out — e.g. codex's `shell` / gemini's `run_shell_command` both map to
    /// `Exec` alongside Claude's `Bash`). It is threaded through the
    /// signature (rather than dropped) so a future runtime whose native
    /// tool names collide with an existing mapping in an incompatible way
    /// can disambiguate without changing every call site.
    pub fn classify(runtime: RuntimeType, tool_name: &str) -> ToolClass {
        let _ = runtime; // reserved — see doc comment above
        let stripped = tool_name
            .strip_prefix(MCP_DUDUCLAW_PREFIX)
            .unwrap_or(tool_name);

        if let Some(class) = classify_native(stripped) {
            return class;
        }
        if let Some(class) = classify_mcp(stripped) {
            return class;
        }
        ToolClass::Other
    }
}

/// Native / CLI built-in tool names (Claude Code's own tool surface, plus a
/// small number of cross-runtime aliases for tools the design explicitly
/// calls out as "the same thing under a different name").
fn classify_native(name: &str) -> Option<ToolClass> {
    Some(match name {
        // Claude Code built-ins
        "Read" | "NotebookRead" => ToolClass::Read,
        "Write" | "Edit" | "MultiEdit" | "NotebookEdit" | "apply_patch" | "write_file" => {
            ToolClass::Write
        }
        "Grep" | "Glob" => ToolClass::Search,
        "Bash" | "BashOutput" | "KillShell" | "SlashCommand" | "shell" | "run_shell_command" => {
            ToolClass::Exec
        }
        "WebFetch" | "WebSearch" => ToolClass::Net,
        "Task" => ToolClass::Delegate,
        "TodoWrite" => ToolClass::TaskBoard,
        _ => return None,
    })
}

/// duduclaw MCP tool names. Enumerated from `crates/duduclaw-cli/src/mcp.rs`
/// `ToolDef` list (207 tools as of 2026-08-06). Grouped by prefix/family
/// where the family shares one class; exceptions are listed individually.
fn classify_mcp(name: &str) -> Option<ToolClass> {
    // ── Family prefixes (checked before the exact-match table so a future
    // new tool under an existing family is classified automatically) ──
    if name.starts_with("memory_") {
        return Some(ToolClass::Memory);
    }
    if name.starts_with("tasks_") || name.starts_with("activity_") {
        return Some(ToolClass::TaskBoard);
    }
    if name.starts_with("capability_") {
        return Some(ToolClass::Approval);
    }
    if name.starts_with("odoo_") {
        return Some(ToolClass::Net);
    }
    if name.starts_with("computer_") {
        return Some(ToolClass::Exec);
    }
    if name.starts_with("gtasks_") {
        return Some(ToolClass::Net);
    }

    Some(match name {
        // ── Agent lifecycle ──
        "spawn_agent" | "spawn_ephemeral" | "send_to_agent" => ToolClass::Delegate,
        "create_agent" | "agent_update" | "agent_update_soul" | "agent_remove" => ToolClass::Write,
        "agent_status" | "list_agents" => ToolClass::Read,

        // ── Wiki (shared_wiki_* / wiki_*) ──
        "wiki_ls" | "wiki_read" | "wiki_stats" | "wiki_graph" | "wiki_trust_audit"
        | "wiki_trust_history" | "wiki_namespace_status" | "shared_wiki_ls" | "shared_wiki_read"
        | "shared_wiki_stats" => ToolClass::Read,
        "wiki_search" | "shared_wiki_search" => ToolClass::Search,
        "wiki_write" | "wiki_dedup" | "wiki_lint" | "wiki_rebuild_fts" | "wiki_share"
        | "wiki_export" | "shared_wiki_write" | "shared_wiki_delete" | "shared_wiki_lint" => {
            ToolClass::Write
        }

        // ── Skills ──
        "skill_search" | "skill_gaps" | "skill_bank_search" => ToolClass::Search,
        "skill_list" | "skill_curator_status" | "skill_synthesis_status" | "shared_skill_list" => {
            ToolClass::Read
        }
        "skill_extract" | "skill_graduate" | "skill_pin" | "skill_from_recording"
        | "skill_hub_install" | "skill_bank_feedback" | "shared_skill_adopt"
        | "shared_skill_share" => ToolClass::Write,
        "skill_security_scan" | "skill_synthesis_run" => ToolClass::Exec,

        // ── Google Workspace / Notion / GitHub external APIs ──
        "gmail_create_draft" | "gmail_read" | "gmail_search" | "calendar_create_event"
        | "calendar_list_events" | "drive_read" | "drive_search" | "docs_append" | "docs_read"
        | "sheets_append" | "sheets_read" | "slides_read" | "forms_get"
        | "forms_list_responses" | "google_status" | "notion_page_append" | "notion_page_read"
        | "notion_search" | "notion_status" | "github_issue_comment" | "github_issue_read"
        | "github_pr_read" | "github_search_issues" | "github_status" => ToolClass::Net,

        // ── Net (web / channel send / OS notify) ──
        "web_extract" | "web_fetch_cached" | "web_search" | "send_message" | "send_photo"
        | "send_sticker" | "os_notify" => ToolClass::Net,

        // ── Decisions / approvals ──
        "decision_resolve" => ToolClass::Approval,
        "decision_list" => ToolClass::Read,

        // ── Task/cron/reminder/plan scheduling (non tasks_*/activity_*) ──
        "create_task" | "schedule_task" | "update_cron_task" | "delete_cron_task"
        | "pause_cron_task" | "create_reminder" | "cancel_reminder" | "plan_start"
        | "plan_update_step" | "goals_create" => ToolClass::Write,
        "list_cron_tasks" | "list_reminders" | "plan_get" | "goals_list" | "task_status"
        | "check_responses" => ToolClass::Read,
        "run_cron_task" => ToolClass::Exec,

        // ── Misc read-only status/query tools ──
        "audit_trail_query" | "autopilot_list" | "channel_config_list" | "channel_status"
        | "cost_agents" | "cost_multi_vs_single" | "cost_recent"
        | "cost_summary" | "cost_users" | "evolution_status" | "fork_cost" | "diff_branches"
        | "inspect_branches" | "hardware_info" | "inference_status" | "llamafile_list"
        | "model_list" | "model_recommend" | "os_calendar_today" | "os_frontmost"
        | "os_watch_status" | "reliability_summary" | "session_restore_context"
        | "user_code_profile" | "user_profile_get" => ToolClass::Read,
        "identity_resolve" | "model_search" | "os_spotlight_search" => ToolClass::Search,

        // ── Config/state mutation ──
        "channel_config" | "evolution_toggle" | "inference_mode" | "jitrl_feedback"
        | "log_mood" | "pairing_manage" | "submit_feedback" | "user_profile_record"
        | "terminate_branch" | "merge_or_select" | "canvas_clear" | "canvas_push" => {
            ToolClass::Write
        }

        // ── Local execution / model lifecycle / media ──
        "execute_program" | "model_download" | "model_load" | "model_unload"
        | "llamafile_start" | "llamafile_stop" | "office_script" | "os_open" | "route_query"
        | "synthesize_speech" | "transcribe_audio" | "fork_run"
        | "browser_record_start" | "browser_record_stop" | "desktop_record_start"
        | "desktop_record_stop" => ToolClass::Exec,

        _ => return None,
    })
}

/// R4 mitigation: whether the `Other` count in a batch exceeds
/// [`OTHER_RATIO_WARN_THRESHOLD`]. Callers should `tracing::warn!` when this
/// returns `true` so an unmapped-tool gap is discoverable rather than a
/// silent quality degradation. Zero total ⇒ `false` (nothing to warn about).
pub fn other_ratio_exceeds_threshold(classes: &[ToolClass]) -> bool {
    if classes.is_empty() {
        return false;
    }
    let other = classes.iter().filter(|c| **c == ToolClass::Other).count();
    (other as f64 / classes.len() as f64) > OTHER_RATIO_WARN_THRESHOLD
}

/// Convenience: classify a batch of `(runtime, tool_name)` pairs into a
/// deduplicated, sorted set of classes. Used by the observation layer to
/// build `observed_tool_classes`.
pub fn classify_set<'a>(
    items: impl IntoIterator<Item = (RuntimeType, &'a str)>,
) -> BTreeSet<ToolClass> {
    items
        .into_iter()
        .map(|(runtime, name)| ToolClass::classify(runtime, name))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Exhaustive coverage over the real MCP tool surface ──
    //
    // Mirrors the `ToolDef` name list in `crates/duduclaw-cli/src/mcp.rs`
    // (207 tools as of 2026-08-06). This is a regression fence: if a new
    // MCP tool is added upstream and NOT added to `classify_mcp`, this test
    // does not fail (that would create a hard cross-crate coupling), but
    // every name below asserts a *specific* non-`Other` class so a future
    // accidental mapping change is caught.
    const ALL_MCP_TOOL_NAMES: &[&str] = &[
        "activity_list", "activity_post", "agent_remove", "agent_status", "agent_update",
        "agent_update_soul", "audit_trail_query", "autopilot_list", "browser_record_start",
        "browser_record_stop", "calendar_create_event", "calendar_list_events",
        "cancel_reminder", "canvas_clear", "canvas_push", "capability_request",
        "channel_config", "channel_config_list", "channel_status", "check_responses",
        "code_map", "computer_click", "computer_key", "computer_screenshot",
        "computer_scroll", "computer_session_start", "computer_session_stop",
        "computer_type", "cost_agents", "cost_multi_vs_single", "cost_recent",
        "cost_summary", "cost_users", "create_agent", "create_reminder", "create_task",
        "decision_list", "decision_resolve", "delete_cron_task", "desktop_record_start",
        "desktop_record_stop", "diff_branches", "docs_append", "docs_read", "drive_read",
        "drive_search", "evolution_status", "evolution_toggle", "execute_program",
        "fork_cost", "fork_run", "forms_get", "forms_list_responses",
        "github_issue_comment", "github_issue_read", "github_pr_read",
        "github_search_issues", "github_status", "gmail_create_draft", "gmail_read",
        "gmail_search", "goals_create", "goals_list", "google_status", "gtasks_complete",
        "gtasks_create", "gtasks_list", "gtasks_lists", "hardware_info", "identity_resolve",
        "inference_mode", "inference_status", "inspect_branches", "jitrl_feedback",
        "list_agents", "list_cron_tasks", "list_reminders", "llamafile_list",
        "llamafile_start", "llamafile_stop", "log_mood", "memory_alias_add",
        "memory_alias_list", "memory_consolidation_status", "memory_episodic_pressure",
        "memory_fetch_batch", "memory_get_at", "memory_get_history", "memory_improve",
        "memory_invalidate_by_origin", "memory_read", "memory_search",
        "memory_search_by_layer", "memory_store", "memory_successful_conversations",
        "merge_or_select", "model_download", "model_list", "model_load", "model_recommend",
        "model_search", "model_unload", "notion_page_append", "notion_page_read",
        "notion_search", "notion_status", "odoo_connect", "odoo_crm_create_lead",
        "odoo_crm_leads", "odoo_crm_update_stage", "odoo_execute", "odoo_inventory_check",
        "odoo_inventory_products", "odoo_invoice_list", "odoo_partner_search",
        "odoo_payment_status", "odoo_report", "odoo_sale_confirm",
        "odoo_sale_create_quotation", "odoo_sale_orders", "odoo_schema_fields",
        "odoo_search", "odoo_status", "office_script", "os_calendar_today",
        "os_frontmost", "os_notify", "os_open", "os_spotlight_search", "os_watch_status",
        "pairing_manage", "pause_cron_task", "plan_get", "plan_start", "plan_update_step",
        "reliability_summary", "route_query", "run_cron_task", "schedule_task",
        "send_message", "send_photo", "send_sticker", "send_to_agent",
        "session_restore_context", "shared_skill_adopt", "shared_skill_list",
        "shared_skill_share", "shared_wiki_delete", "shared_wiki_lint", "shared_wiki_ls",
        "shared_wiki_read", "shared_wiki_search", "shared_wiki_stats", "shared_wiki_write",
        "sheets_append", "sheets_read", "skill_bank_feedback", "skill_bank_search",
        "skill_curator_status", "skill_extract", "skill_from_recording", "skill_gaps",
        "skill_graduate", "skill_hub_install", "skill_list", "skill_pin", "skill_search",
        "skill_security_scan", "skill_synthesis_run", "skill_synthesis_status",
        "slides_read", "spawn_agent", "spawn_ephemeral", "submit_feedback",
        "synthesize_speech", "task_status", "tasks_block", "tasks_claim", "tasks_complete",
        "tasks_create", "tasks_list", "tasks_renew", "tasks_update", "terminate_branch",
        "transcribe_audio", "update_cron_task", "user_code_profile", "user_profile_get",
        "user_profile_record", "web_extract", "web_fetch_cached", "web_search",
        "wiki_dedup", "wiki_export", "wiki_graph", "wiki_lint", "wiki_ls",
        "wiki_namespace_status", "wiki_read", "wiki_rebuild_fts", "wiki_search",
        "wiki_share", "wiki_stats", "wiki_trust_audit", "wiki_trust_history", "wiki_write",
        // Tools deliberately left unmapped today (no single obvious class,
        // low-traffic in the goal loop dispatch path) — asserted `Other`
        // explicitly below so a future accidental mapping isn't silently
        // untested either way.
        "code_map",
    ];

    #[test]
    fn classify_exhaustive_mcp_surface_has_no_silent_other() {
        let mut unmapped = Vec::new();
        for name in ALL_MCP_TOOL_NAMES {
            if *name == "code_map" {
                continue; // asserted separately below
            }
            let class = ToolClass::classify(RuntimeType::Claude, name);
            if class == ToolClass::Other {
                unmapped.push(*name);
            }
        }
        assert!(
            unmapped.is_empty(),
            "these MCP tool names fell through to Other, update classify_mcp: {unmapped:?}"
        );
    }

    #[test]
    fn classify_code_map_is_currently_other_documented() {
        // `code_map` has no clean class (it's a static analysis summary tool,
        // not quite Read/Search); documented as a known Other today rather
        // than forced into a bucket that doesn't fit.
        assert_eq!(
            ToolClass::classify(RuntimeType::Claude, "code_map"),
            ToolClass::Other
        );
    }

    #[test]
    fn classify_strips_exact_mcp_duduclaw_prefix() {
        assert_eq!(
            ToolClass::classify(RuntimeType::Claude, "mcp__duduclaw__tasks_create"),
            ToolClass::TaskBoard
        );
        assert_eq!(
            ToolClass::classify(RuntimeType::Claude, "mcp__duduclaw__memory_search"),
            ToolClass::Memory
        );
    }

    #[test]
    fn classify_does_not_strip_unanchored_or_foreign_prefix() {
        // A different MCP server's prefix (e.g. Playwright) must NOT be
        // treated as duduclaw's — no unanchored substring match (convention
        // #2). It should fall through to Other, not be misclassified.
        assert_eq!(
            ToolClass::classify(RuntimeType::Claude, "mcp__playwright__browser_click"),
            ToolClass::Other
        );
        // A tool name that merely *contains* the prefix mid-string must not
        // be stripped either.
        assert_eq!(
            ToolClass::classify(RuntimeType::Claude, "xmcp__duduclaw__tasks_create"),
            ToolClass::Other
        );
    }

    #[test]
    fn classify_native_claude_code_tools() {
        assert_eq!(ToolClass::classify(RuntimeType::Claude, "Read"), ToolClass::Read);
        assert_eq!(ToolClass::classify(RuntimeType::Claude, "Write"), ToolClass::Write);
        assert_eq!(ToolClass::classify(RuntimeType::Claude, "Edit"), ToolClass::Write);
        assert_eq!(ToolClass::classify(RuntimeType::Claude, "Grep"), ToolClass::Search);
        assert_eq!(ToolClass::classify(RuntimeType::Claude, "Glob"), ToolClass::Search);
        assert_eq!(ToolClass::classify(RuntimeType::Claude, "Bash"), ToolClass::Exec);
        assert_eq!(ToolClass::classify(RuntimeType::Claude, "WebFetch"), ToolClass::Net);
        assert_eq!(ToolClass::classify(RuntimeType::Claude, "WebSearch"), ToolClass::Net);
        assert_eq!(ToolClass::classify(RuntimeType::Claude, "Task"), ToolClass::Delegate);
        assert_eq!(ToolClass::classify(RuntimeType::Claude, "TodoWrite"), ToolClass::TaskBoard);
    }

    #[test]
    fn classify_cross_runtime_exec_aliases_agree() {
        // Claude's Bash, codex's shell, gemini's run_shell_command are "the
        // same thing" per the design rationale — must land in the same class
        // regardless of which runtime produced the name.
        assert_eq!(ToolClass::classify(RuntimeType::Claude, "Bash"), ToolClass::Exec);
        assert_eq!(ToolClass::classify(RuntimeType::Codex, "shell"), ToolClass::Exec);
        assert_eq!(
            ToolClass::classify(RuntimeType::Gemini, "run_shell_command"),
            ToolClass::Exec
        );
    }

    #[test]
    fn classify_unknown_tool_is_other() {
        assert_eq!(
            ToolClass::classify(RuntimeType::Claude, "totally_unknown_tool_xyz"),
            ToolClass::Other
        );
        assert_eq!(ToolClass::classify(RuntimeType::Codex, ""), ToolClass::Other);
    }

    #[test]
    fn other_ratio_threshold_empty_is_false() {
        assert!(!other_ratio_exceeds_threshold(&[]));
    }

    #[test]
    fn other_ratio_threshold_below_and_above() {
        let below = vec![ToolClass::Other, ToolClass::Read, ToolClass::Read, ToolClass::Read, ToolClass::Read];
        assert!(!other_ratio_exceeds_threshold(&below)); // 1/5 = 0.20, not > 0.20

        let above = vec![ToolClass::Other, ToolClass::Other, ToolClass::Read, ToolClass::Read];
        assert!(other_ratio_exceeds_threshold(&above)); // 2/4 = 0.50
    }

    #[test]
    fn classify_set_dedupes_and_sorts() {
        let items = vec![
            (RuntimeType::Claude, "Bash"),
            (RuntimeType::Claude, "Bash"),
            (RuntimeType::Claude, "Read"),
            (RuntimeType::Claude, "tasks_create"),
        ];
        let set = classify_set(items);
        assert_eq!(set.len(), 3);
        assert!(set.contains(&ToolClass::Exec));
        assert!(set.contains(&ToolClass::Read));
        assert!(set.contains(&ToolClass::TaskBoard));
    }
}
