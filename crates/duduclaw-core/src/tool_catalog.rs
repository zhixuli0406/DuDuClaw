//! Built-in tool catalog — the single, dependency-neutral source of truth for
//! the tools an agent can be granted or denied via `[capabilities] allowed_tools`
//! / `denied_tools`.
//!
//! ## Why this lives in `duduclaw-core`
//!
//! The authoritative MCP tool → scope enumeration is
//! `duduclaw_cli::mcp_auth::tool_requires_scope`. The dashboard, however, needs
//! the same list to render the "add from built-in tools" picker, and the
//! dashboard RPC is served by `duduclaw-gateway`. The crate dependency runs
//! `duduclaw-cli` → `duduclaw-gateway` → `duduclaw-core`, so the gateway can
//! **not** import the cli crate (that would be a dependency cycle). The only
//! crate both the cli and the gateway can reach is `duduclaw-core`.
//!
//! Therefore the catalog table lives here. To keep it from silently drifting
//! from the security gate, `duduclaw-cli` carries a test
//! (`test_catalog_scopes_match_tool_requires_scope`) that asserts every MCP
//! entry's [`ToolCatalogEntry::scope`] equals the scope the gate actually
//! enforces for that tool. If the two ever diverge, that test fails the build.
//!
//! Category is a UI-only grouping and is intentionally independent of scope —
//! e.g. `wiki_trust_audit` groups under `wiki` for the picker but its scope is
//! `admin`, matching the gate.

use serde::Serialize;

/// The MCP server name DuDuClaw registers under in the Claude CLI config. An
/// MCP tool `foo` is referenced in a `--allowedTools` / `--disallowedTools`
/// allowlist as `mcp__duduclaw__foo`. Kept in sync with the server
/// registration in `duduclaw-cli` (`mcp.rs`, server name "duduclaw").
pub const MCP_SERVER_NAME: &str = "duduclaw";

/// One entry in the built-in tool catalog surfaced to the dashboard capability
/// editor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ToolCatalogEntry {
    /// Bare tool name, e.g. `office_script` or `Bash`.
    pub name: &'static str,
    /// The exact string to add to `[capabilities] allowed_tools` /
    /// `denied_tools` (Claude CLI `--allowedTools` syntax). MCP tools become
    /// `mcp__duduclaw__<name>`; native Claude tools use the bare name.
    pub qualified: String,
    /// Human-readable one-liner for the picker.
    pub description: &'static str,
    /// MCP scope string (the `mcp_auth::Scope` Display form, e.g. `skill:execute`)
    /// for MCP tools, or empty for native Claude tools which have no MCP scope.
    pub scope: &'static str,
    /// UI grouping category (`channel` / `memory` / `agent` / `skill` / `task`
    /// / `wiki` / `odoo` / `office` / `cron` / ...). UI-only; independent of
    /// [`Self::scope`].
    pub category: &'static str,
    /// `mcp` for DuDuClaw MCP tools, `claude` for native Claude Code tools.
    pub kind: &'static str,
}

/// Static table of DuDuClaw MCP tools: `(name, description, scope, category)`.
///
/// This mirrors — and must stay equal to — the explicitly-enumerated tools in
/// `duduclaw_cli::mcp_auth::tool_requires_scope`. The cli-side drift test is
/// the mechanical guard (see the module docs). `scope` is always the Display
/// form of the enforced `Scope`; admin-tier tools carry `admin` even when their
/// `category` groups them elsewhere.
const MCP_TOOLS: &[(&str, &str, &str, &str)] = &[
    // ── Channel / messaging egress (messaging:send) ──────────────────────
    (
        "send_message",
        "Send a message to a channel (Telegram/LINE/Discord)",
        "messaging:send",
        "channel",
    ),
    (
        "send_photo",
        "Send a photo to a channel",
        "messaging:send",
        "channel",
    ),
    (
        "send_sticker",
        "Send a sticker (LINE)",
        "messaging:send",
        "channel",
    ),
    (
        "synthesize_speech",
        "Text-to-speech synthesis",
        "messaging:send",
        "channel",
    ),
    (
        "transcribe_audio",
        "Transcribe audio to text",
        "messaging:send",
        "channel",
    ),
    // ── Memory: read (memory:read) ───────────────────────────────────────
    (
        "memory_search",
        "Search agent memory",
        "memory:read",
        "memory",
    ),
    (
        "memory_read",
        "Read a memory entry",
        "memory:read",
        "memory",
    ),
    (
        "memory_fetch_batch",
        "Fetch up to 100 memory entries by ID",
        "memory:read",
        "memory",
    ),
    (
        "memory_get_history",
        "Fact supersession chain (temporal memory)",
        "memory:read",
        "memory",
    ),
    (
        "memory_get_at",
        "Point-in-time fact lookup",
        "memory:read",
        "memory",
    ),
    (
        "memory_alias_list",
        "List entity aliases",
        "memory:read",
        "memory",
    ),
    (
        "memory_search_by_layer",
        "Search a specific memory layer",
        "memory:read",
        "memory",
    ),
    (
        "memory_successful_conversations",
        "List past successful conversations",
        "memory:read",
        "memory",
    ),
    (
        "memory_consolidation_status",
        "Memory consolidation status",
        "memory:read",
        "memory",
    ),
    (
        "memory_improve",
        "Trigger a memory improvement pass",
        "memory:read",
        "memory",
    ),
    (
        "memory_episodic_pressure",
        "Episodic memory pressure metrics",
        "memory:read",
        "memory",
    ),
    (
        "user_profile_get",
        "Read the stored user profile",
        "memory:read",
        "memory",
    ),
    (
        "user_code_profile",
        "Read the user's code/tech profile",
        "memory:read",
        "memory",
    ),
    (
        "code_map",
        "Repository code map (Aider-style)",
        "memory:read",
        "memory",
    ),
    // ── Memory: write (memory:write) ─────────────────────────────────────
    (
        "memory_store",
        "Store a memory entry",
        "memory:write",
        "memory",
    ),
    (
        "memory_alias_add",
        "Add an entity alias",
        "memory:write",
        "memory",
    ),
    (
        "user_profile_record",
        "Record a user-profile fact",
        "memory:write",
        "memory",
    ),
    // ── Wiki: read (wiki:read) ───────────────────────────────────────────
    ("wiki_read", "Read a wiki page", "wiki:read", "wiki"),
    ("wiki_search", "Search wiki pages", "wiki:read", "wiki"),
    ("wiki_ls", "List wiki pages", "wiki:read", "wiki"),
    ("wiki_stats", "Wiki statistics", "wiki:read", "wiki"),
    ("wiki_export", "Export wiki pages", "wiki:read", "wiki"),
    ("wiki_graph", "Wiki link graph", "wiki:read", "wiki"),
    ("wiki_lint", "Wiki health check", "wiki:read", "wiki"),
    (
        "wiki_namespace_status",
        "Shared-wiki namespace policy status",
        "wiki:read",
        "wiki",
    ),
    (
        "shared_wiki_read",
        "Read a shared wiki page",
        "wiki:read",
        "wiki",
    ),
    (
        "shared_wiki_search",
        "Search shared wiki",
        "wiki:read",
        "wiki",
    ),
    (
        "shared_wiki_ls",
        "List shared wiki pages",
        "wiki:read",
        "wiki",
    ),
    (
        "shared_wiki_stats",
        "Shared wiki statistics",
        "wiki:read",
        "wiki",
    ),
    (
        "shared_wiki_lint",
        "Shared wiki health check",
        "wiki:read",
        "wiki",
    ),
    // ── Wiki: write (wiki:write) ─────────────────────────────────────────
    ("wiki_write", "Write a wiki page", "wiki:write", "wiki"),
    (
        "wiki_share",
        "Share a wiki page to the shared wiki",
        "wiki:write",
        "wiki",
    ),
    ("wiki_dedup", "Deduplicate wiki pages", "wiki:write", "wiki"),
    (
        "wiki_rebuild_fts",
        "Rebuild the wiki full-text index",
        "wiki:write",
        "wiki",
    ),
    (
        "shared_wiki_write",
        "Write a shared wiki page",
        "wiki:write",
        "wiki",
    ),
    (
        "shared_wiki_delete",
        "Delete a shared wiki page",
        "wiki:write",
        "wiki",
    ),
    (
        "canvas_push",
        "Push presentation content to the Live Canvas",
        "wiki:write",
        "wiki",
    ),
    (
        "canvas_clear",
        "Clear the Live Canvas",
        "wiki:write",
        "wiki",
    ),
    // ── Identity (identity:read) ─────────────────────────────────────────
    (
        "identity_resolve",
        "Resolve a person to a canonical record",
        "identity:read",
        "identity",
    ),
    // ── Odoo: read (odoo:read) ───────────────────────────────────────────
    (
        "odoo_status",
        "Odoo connection diagnostics",
        "odoo:read",
        "odoo",
    ),
    ("odoo_crm_leads", "List CRM leads", "odoo:read", "odoo"),
    ("odoo_sale_orders", "List sale orders", "odoo:read", "odoo"),
    (
        "odoo_inventory_products",
        "List inventory products",
        "odoo:read",
        "odoo",
    ),
    (
        "odoo_inventory_check",
        "Check inventory levels",
        "odoo:read",
        "odoo",
    ),
    ("odoo_invoice_list", "List invoices", "odoo:read", "odoo"),
    (
        "odoo_payment_status",
        "Payment status lookup",
        "odoo:read",
        "odoo",
    ),
    (
        "odoo_partner_search",
        "Search partners",
        "odoo:read",
        "odoo",
    ),
    (
        "odoo_schema_fields",
        "Inspect model fields",
        "odoo:read",
        "odoo",
    ),
    (
        "odoo_search",
        "Generic Odoo search_read",
        "odoo:read",
        "odoo",
    ),
    (
        "odoo_connect",
        "Acquire/refresh the Odoo connection",
        "odoo:read",
        "odoo",
    ),
    // ── Odoo: write (odoo:write) ─────────────────────────────────────────
    (
        "odoo_crm_create_lead",
        "Create a CRM lead",
        "odoo:write",
        "odoo",
    ),
    (
        "odoo_crm_update_stage",
        "Update a CRM lead stage",
        "odoo:write",
        "odoo",
    ),
    (
        "odoo_sale_create_quotation",
        "Create a sale quotation",
        "odoo:write",
        "odoo",
    ),
    // ── Odoo: execute (odoo:execute) ─────────────────────────────────────
    (
        "odoo_sale_confirm",
        "Confirm a sale order (workflow)",
        "odoo:execute",
        "odoo",
    ),
    (
        "odoo_execute",
        "Generic Odoo execute_kw",
        "odoo:execute",
        "odoo",
    ),
    (
        "odoo_report",
        "Generate an Odoo report",
        "odoo:execute",
        "odoo",
    ),
    // ── Google Workspace: read (google:read) ─────────────────────────────
    (
        "google_status",
        "Google connection diagnostics",
        "google:read",
        "google",
    ),
    ("gmail_search", "Search Gmail", "google:read", "google"),
    (
        "gmail_read",
        "Read a Gmail message",
        "google:read",
        "google",
    ),
    (
        "calendar_list_events",
        "List calendar events",
        "google:read",
        "google",
    ),
    (
        "sheets_read",
        "Read a spreadsheet range",
        "google:read",
        "google",
    ),
    // ── Google Workspace: write (google:write) ───────────────────────────
    (
        "gmail_create_draft",
        "Create a Gmail draft (never sends)",
        "google:write",
        "google",
    ),
    (
        "calendar_create_event",
        "Create a calendar event",
        "google:write",
        "google",
    ),
    (
        "sheets_append",
        "Append a spreadsheet row",
        "google:write",
        "google",
    ),
    // ── Notion: read (notion:read) ───────────────────────────────────────
    (
        "notion_status",
        "Notion connection diagnostics",
        "notion:read",
        "notion",
    ),
    ("notion_search", "Search Notion", "notion:read", "notion"),
    (
        "notion_page_read",
        "Read a Notion page",
        "notion:read",
        "notion",
    ),
    // ── Notion: write (notion:write) ─────────────────────────────────────
    (
        "notion_page_append",
        "Append blocks to a Notion page",
        "notion:write",
        "notion",
    ),
    // ── GitHub: read (github:read) ───────────────────────────────────────
    (
        "github_status",
        "GitHub connection diagnostics",
        "github:read",
        "github",
    ),
    (
        "github_search_issues",
        "Search GitHub issues",
        "github:read",
        "github",
    ),
    (
        "github_issue_read",
        "Read a GitHub issue",
        "github:read",
        "github",
    ),
    (
        "github_pr_read",
        "Read a GitHub pull request",
        "github:read",
        "github",
    ),
    // ── GitHub: write (github:write) ─────────────────────────────────────
    (
        "github_issue_comment",
        "Comment on a GitHub issue/PR",
        "github:write",
        "github",
    ),
    // ── Live Run Forking (fork:execute) ──────────────────────────────────
    (
        "fork_run",
        "Fork a run into parallel branches",
        "fork:execute",
        "fork",
    ),
    (
        "inspect_branches",
        "Inspect fork branches",
        "fork:execute",
        "fork",
    ),
    (
        "diff_branches",
        "Diff two fork branches",
        "fork:execute",
        "fork",
    ),
    (
        "merge_or_select",
        "Merge/select a fork branch",
        "fork:execute",
        "fork",
    ),
    (
        "terminate_branch",
        "Terminate a fork branch",
        "fork:execute",
        "fork",
    ),
    ("fork_cost", "Fork run cost summary", "fork:execute", "fork"),
    // ── OS-native (os:native) ────────────────────────────────────────────
    (
        "os_notify",
        "Send a native OS notification",
        "os:native",
        "os",
    ),
    (
        "os_watch_status",
        "OS filesystem watch status",
        "os:native",
        "os",
    ),
    (
        "os_open",
        "Open a file/URL with the OS default handler",
        "os:native",
        "os",
    ),
    ("os_frontmost", "Frontmost app/window", "os:native", "os"),
    ("os_spotlight_search", "Spotlight search", "os:native", "os"),
    (
        "os_calendar_today",
        "Today's calendar events",
        "os:native",
        "os",
    ),
    // ── Office document scripting (skill:execute) ────────────────────────
    (
        "office_script",
        "Run a bundled office document script (docx/xlsx/pptx/pdf)",
        "skill:execute",
        "office",
    ),
    // ── Recording → skill capture (recording; WP3.3) ─────────────────────
    // All five ALSO require `[capabilities] recording = true` at the dispatch
    // gate (deny-by-default).
    (
        "browser_record_start",
        "Start a browser recording session (Playwright trace + HAR)",
        "recording",
        "recording",
    ),
    (
        "browser_record_stop",
        "Stop a browser recording and persist redacted artifacts",
        "recording",
        "recording",
    ),
    (
        "desktop_record_start",
        "Start a desktop recording (screenshots + foreground window)",
        "recording",
        "recording",
    ),
    (
        "desktop_record_stop",
        "Stop a desktop recording session",
        "recording",
        "recording",
    ),
    (
        "skill_from_recording",
        "Distill a recording into a draft SKILL.md (approval-gated)",
        "recording",
        "recording",
    ),
    // ── Admin-tier tools (admin) ─────────────────────────────────────────
    (
        "memory_invalidate_by_origin",
        "Destructive: expire all facts from one source",
        "admin",
        "memory",
    ),
    (
        "execute_program",
        "Run an arbitrary program",
        "admin",
        "system",
    ),
    ("create_agent", "Create a sub-agent", "admin", "agent"),
    ("spawn_agent", "Spawn a sub-agent run", "admin", "agent"),
    (
        "spawn_ephemeral",
        "Spawn an ephemeral synthesized agent",
        "admin",
        "agent",
    ),
    ("agent_update", "Update an agent's config", "admin", "agent"),
    (
        "agent_update_soul",
        "Rewrite an agent's SOUL.md",
        "admin",
        "agent",
    ),
    ("agent_remove", "Remove an agent", "admin", "agent"),
    (
        "send_to_agent",
        "Delegate a task to another agent",
        "admin",
        "agent",
    ),
    (
        "evolution_toggle",
        "Toggle the evolution engine",
        "admin",
        "system",
    ),
    ("schedule_task", "Schedule a cron task", "admin", "cron"),
    ("delete_cron_task", "Delete a cron task", "admin", "cron"),
    ("update_cron_task", "Update a cron task", "admin", "cron"),
    ("pause_cron_task", "Pause a cron task", "admin", "cron"),
    ("run_cron_task", "Run a cron task once now", "admin", "cron"),
    ("channel_config", "Configure a channel", "admin", "channel"),
    (
        "model_download",
        "Download a local model",
        "admin",
        "inference",
    ),
    ("model_load", "Load a local model", "admin", "inference"),
    ("model_unload", "Unload a local model", "admin", "inference"),
    (
        "llamafile_start",
        "Start a llamafile server",
        "admin",
        "inference",
    ),
    (
        "llamafile_stop",
        "Stop a llamafile server",
        "admin",
        "inference",
    ),
    (
        "inference_mode",
        "Switch the inference mode",
        "admin",
        "inference",
    ),
    (
        "jitrl_feedback",
        "Record local-inference feedback",
        "admin",
        "inference",
    ),
    (
        "cost_multi_vs_single",
        "Multi- vs single-agent cost comparison",
        "admin",
        "system",
    ),
    (
        "skill_extract",
        "Extract a skill from episodic memory",
        "admin",
        "skill",
    ),
    ("skill_graduate", "Graduate a trial skill", "admin", "skill"),
    (
        "skill_security_scan",
        "Security-scan a skill",
        "admin",
        "skill",
    ),
    (
        "skill_synthesis_run",
        "Run the skill synthesis pipeline",
        "admin",
        "skill",
    ),
    (
        "shared_skill_adopt",
        "Adopt a shared skill",
        "admin",
        "skill",
    ),
    (
        "shared_skill_share",
        "Share a skill cross-agent",
        "admin",
        "skill",
    ),
    (
        "audit_trail_query",
        "Query the audit trail",
        "admin",
        "security",
    ),
    (
        "reliability_summary",
        "Reliability dashboard summary",
        "admin",
        "security",
    ),
    (
        "wiki_trust_audit",
        "Wiki page-level trust trends",
        "admin",
        "wiki",
    ),
    ("wiki_trust_history", "Wiki trust history", "admin", "wiki"),
    // ── 2026-07-28 coverage completion ───────────────────────────────────
    // Every tool advertised by the MCP server's tools/list that was missing
    // from the catalog. All of these fall through to `Scope::Admin` in
    // `tool_requires_scope` (the C2 fail-closed default) — the catalog must
    // mirror the gate as it IS, not as it might be; if a tool later gets a
    // least-privilege scope in mcp_auth, update its entry here (the cli drift
    // test will fail the build until both agree). `category` remains UI-only.
    // ── Task board / goals / plans (task) ────────────────────────────────
    (
        "tasks_list",
        "List tasks from the shared Kanban board",
        "admin",
        "task",
    ),
    (
        "tasks_create",
        "Create a Kanban board task",
        "admin",
        "task",
    ),
    ("tasks_update", "Update board task fields", "admin", "task"),
    (
        "tasks_claim",
        "Atomically claim a pending board task",
        "admin",
        "task",
    ),
    ("tasks_complete", "Mark a board task done", "admin", "task"),
    ("tasks_block", "Mark a board task blocked", "admin", "task"),
    (
        "tasks_renew",
        "Renew a claimed task's lease",
        "admin",
        "task",
    ),
    (
        "activity_list",
        "List recent Activity Feed events",
        "admin",
        "task",
    ),
    (
        "activity_post",
        "Post progress to the Activity Feed",
        "admin",
        "task",
    ),
    (
        "goals_create",
        "Create a goal in the goal hierarchy",
        "admin",
        "task",
    ),
    ("goals_list", "List goals in the hierarchy", "admin", "task"),
    (
        "plan_get",
        "Read the shared co-edited plan",
        "admin",
        "task",
    ),
    (
        "plan_start",
        "Clarify-first planning for an ambiguous task",
        "admin",
        "task",
    ),
    (
        "plan_update_step",
        "Update your steps in a shared plan",
        "admin",
        "task",
    ),
    // ── Web access (web) ─────────────────────────────────────────────────
    ("web_search", "Search the web", "admin", "web"),
    (
        "web_fetch_cached",
        "Fetch a URL (SSRF-guarded, cached, rate-limited)",
        "admin",
        "web",
    ),
    (
        "web_extract",
        "Fetch a URL and extract elements via CSS selector",
        "admin",
        "web",
    ),
    // ── Cost telemetry (cost) ────────────────────────────────────────────
    (
        "cost_summary",
        "Token usage and cost summary",
        "admin",
        "cost",
    ),
    ("cost_agents", "Agents ranked by cost", "admin", "cost"),
    ("cost_recent", "Recent API call records", "admin", "cost"),
    ("cost_users", "End users ranked by cost", "admin", "cost"),
    // ── Computer use actions (computer) ──────────────────────────────────
    // Additionally gated by `[capabilities] computer_use` at dispatch.
    (
        "computer_session_start",
        "Start a Computer Use session",
        "admin",
        "computer",
    ),
    (
        "computer_session_stop",
        "Stop a Computer Use session",
        "admin",
        "computer",
    ),
    (
        "computer_screenshot",
        "Capture a screenshot",
        "admin",
        "computer",
    ),
    (
        "computer_click",
        "Click at screen coordinates",
        "admin",
        "computer",
    ),
    (
        "computer_type",
        "Type text at the cursor",
        "admin",
        "computer",
    ),
    (
        "computer_key",
        "Press a key combination",
        "admin",
        "computer",
    ),
    (
        "computer_scroll",
        "Scroll at screen coordinates",
        "admin",
        "computer",
    ),
    // ── Agent roster / delegation status (agent) ─────────────────────────
    (
        "list_agents",
        "List registered agents with hierarchy",
        "admin",
        "agent",
    ),
    (
        "agent_status",
        "Detailed status of one agent",
        "admin",
        "agent",
    ),
    (
        "create_task",
        "Submit a multi-step task to the dispatcher",
        "admin",
        "agent",
    ),
    (
        "task_status",
        "Status of a dispatched task",
        "admin",
        "agent",
    ),
    (
        "check_responses",
        "Check replies from agents you delegated to",
        "admin",
        "agent",
    ),
    // ── Scheduling / reminders (cron) ────────────────────────────────────
    (
        "list_cron_tasks",
        "List scheduled cron tasks",
        "admin",
        "cron",
    ),
    (
        "create_reminder",
        "Create a one-shot reminder",
        "admin",
        "cron",
    ),
    (
        "cancel_reminder",
        "Cancel a pending reminder",
        "admin",
        "cron",
    ),
    ("list_reminders", "List reminders", "admin", "cron"),
    // ── Channel status (channel) ─────────────────────────────────────────
    (
        "channel_status",
        "Per-channel connection state overview",
        "admin",
        "channel",
    ),
    (
        "channel_config_list",
        "List channel settings for a scope",
        "admin",
        "channel",
    ),
    // ── Skill ecosystem (skill) ──────────────────────────────────────────
    (
        "skill_search",
        "Search skill hubs for installable skills",
        "admin",
        "skill",
    ),
    (
        "skill_list",
        "List skills installed for an agent",
        "admin",
        "skill",
    ),
    (
        "skill_hub_install",
        "Install a skill from a configured hub",
        "admin",
        "skill",
    ),
    (
        "skill_pin",
        "Pin/unpin a skill for the curator",
        "admin",
        "skill",
    ),
    (
        "skill_gaps",
        "Report inferred capability gaps",
        "admin",
        "skill",
    ),
    (
        "skill_bank_search",
        "Search learned skills in the skill bank",
        "admin",
        "skill",
    ),
    (
        "skill_bank_feedback",
        "Record skill execution feedback",
        "admin",
        "skill",
    ),
    (
        "skill_curator_status",
        "Skill curator lifecycle state",
        "admin",
        "skill",
    ),
    (
        "skill_synthesis_status",
        "Skill auto-synthesis status",
        "admin",
        "skill",
    ),
    (
        "shared_skill_list",
        "List team-shared skills",
        "admin",
        "skill",
    ),
    // ── Local inference / models (inference) ─────────────────────────────
    ("model_list", "List local GGUF models", "admin", "inference"),
    (
        "model_search",
        "Search downloadable GGUF models",
        "admin",
        "inference",
    ),
    (
        "model_recommend",
        "Hardware-aware model recommendations",
        "admin",
        "inference",
    ),
    (
        "route_query",
        "Preview the confidence-router decision",
        "admin",
        "inference",
    ),
    (
        "inference_status",
        "Local inference engine status",
        "admin",
        "inference",
    ),
    (
        "hardware_info",
        "Detect hardware capabilities",
        "admin",
        "inference",
    ),
    (
        "llamafile_list",
        "List available llamafiles",
        "admin",
        "inference",
    ),
    // ── Platform / misc (system) ─────────────────────────────────────────
    (
        "autopilot_list",
        "List automation rules (read-only)",
        "admin",
        "system",
    ),
    (
        "pairing_manage",
        "Manage channel pairing requests",
        "admin",
        "system",
    ),
    (
        "capability_request",
        "Request a task-scoped tool grant (HITL)",
        "admin",
        "system",
    ),
    (
        "decision_list",
        "List your open decisions",
        "admin",
        "system",
    ),
    (
        "decision_resolve",
        "Resolve an open decision",
        "admin",
        "system",
    ),
    (
        "evolution_status",
        "Evolution engine status",
        "admin",
        "system",
    ),
    (
        "submit_feedback",
        "Submit a user feedback signal",
        "admin",
        "system",
    ),
    ("log_mood", "Log user mood", "admin", "system"),
    (
        "session_restore_context",
        "Search archived session messages",
        "admin",
        "system",
    ),
];

/// Native Claude Code tools: `(name, description)`. These are defined by Claude
/// Code (not DuDuClaw), have no MCP scope, and are referenced by their bare
/// name in the allowlist. Included so the picker can offer them alongside MCP
/// tools; excluded from the cli scope-drift test (they are not MCP tools).
const CLAUDE_TOOLS: &[(&str, &str)] = &[
    ("Bash", "Run shell commands"),
    ("Read", "Read a file"),
    ("Write", "Write a file"),
    ("Edit", "Edit a file"),
    ("Glob", "Find files by glob pattern"),
    ("Grep", "Search file contents"),
    ("WebFetch", "Fetch and read a URL"),
    ("WebSearch", "Search the web"),
    ("TodoWrite", "Manage the task todo list"),
    ("Task", "Spawn a Claude sub-agent"),
    ("NotebookEdit", "Edit a Jupyter notebook"),
];

/// Build the full built-in tool catalog: every DuDuClaw MCP tool (qualified as
/// `mcp__duduclaw__<name>`) followed by the native Claude Code tools (bare
/// name). This is the single list the dashboard `tools.builtin_catalog` RPC
/// returns.
pub fn builtin_tool_catalog() -> Vec<ToolCatalogEntry> {
    let mut out = Vec::with_capacity(MCP_TOOLS.len() + CLAUDE_TOOLS.len());
    for &(name, description, scope, category) in MCP_TOOLS {
        out.push(ToolCatalogEntry {
            name,
            qualified: format!("mcp__{MCP_SERVER_NAME}__{name}"),
            description,
            scope,
            category,
            kind: "mcp",
        });
    }
    for &(name, description) in CLAUDE_TOOLS {
        out.push(ToolCatalogEntry {
            name,
            qualified: name.to_string(),
            description,
            scope: "",
            category: "claude",
            kind: "claude",
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn test_catalog_is_non_empty() {
        assert!(!builtin_tool_catalog().is_empty());
    }

    #[test]
    fn test_catalog_contains_office_script() {
        let catalog = builtin_tool_catalog();
        let office = catalog
            .iter()
            .find(|e| e.name == "office_script")
            .expect("office_script must be in the catalog");
        assert_eq!(office.qualified, "mcp__duduclaw__office_script");
        assert_eq!(office.scope, "skill:execute");
        assert_eq!(office.kind, "mcp");
        assert_eq!(office.category, "office");
    }

    #[test]
    fn test_catalog_has_no_duplicate_names() {
        let catalog = builtin_tool_catalog();
        let mut seen = HashSet::new();
        for e in &catalog {
            assert!(seen.insert(e.name), "duplicate tool name: {}", e.name);
        }
    }

    #[test]
    fn test_catalog_has_no_duplicate_qualified() {
        let catalog = builtin_tool_catalog();
        let mut seen = HashSet::new();
        for e in &catalog {
            assert!(
                seen.insert(e.qualified.clone()),
                "duplicate qualified name: {}",
                e.qualified
            );
        }
    }

    #[test]
    fn test_native_claude_tools_use_bare_qualified_name() {
        let catalog = builtin_tool_catalog();
        let bash = catalog.iter().find(|e| e.name == "Bash").unwrap();
        assert_eq!(bash.qualified, "Bash");
        assert_eq!(bash.kind, "claude");
        assert_eq!(bash.scope, "");
    }

    #[test]
    fn test_mcp_entries_are_qualified() {
        for e in builtin_tool_catalog().iter().filter(|e| e.kind == "mcp") {
            assert!(
                e.qualified.starts_with("mcp__duduclaw__"),
                "MCP tool {} must be qualified",
                e.name
            );
            assert!(!e.scope.is_empty(), "MCP tool {} must have a scope", e.name);
        }
    }
}
