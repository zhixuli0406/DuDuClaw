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
    /// T5 (feature audit 2026-09-29): this tool name is a **deprecated alias**
    /// for a merged entry point. It is still listed and still callable — MCP's
    /// `tools/list` is the declaration surface, so hiding a tool makes it
    /// *uncallable*, which is the opposite of what a deprecation window is for
    /// — but the dashboard should not offer it as a new choice, and its
    /// description carries a `[deprecated → …]` prefix. The old→new table is
    /// `docs/guides/deprecations.md`; removed names are [`REMOVED_MCP_TOOLS`].
    pub deprecated: bool,
}

/// MCP tool names that are deprecated aliases of a merged entry point: still
/// listed and callable, flagged `deprecated: true` in the catalog. Empty since
/// v1.69.0 removed the last window's aliases (see [`REMOVED_MCP_TOOLS`]); kept
/// so the next deprecation is a one-line diff.
pub const DEPRECATED_MCP_TOOLS: &[&str] = &[];

/// Is this MCP tool name a deprecated alias? Exact match, never a prefix or
/// substring test (coding convention 2).
pub fn is_deprecated_tool(name: &str) -> bool {
    DEPRECATED_MCP_TOOLS.contains(&name)
}

/// An MCP tool name that was removed after its deprecation window, with the
/// call that replaces it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RemovedMcpTool {
    /// The removed tool name.
    pub name: &'static str,
    /// Release that removed it, without the `v` prefix.
    pub removed_in: &'static str,
    /// The tool to call instead.
    pub replacement: &'static str,
    /// The argument that selects the removed tool's behaviour on
    /// [`Self::replacement`], written as the model should type it.
    pub replacement_args: &'static str,
    /// Extra guidance for the caller, or empty.
    pub note: &'static str,
}

impl RemovedMcpTool {
    /// The error text a caller of the removed name receives. English on
    /// purpose: it is read by the model, like every other tool error.
    pub fn message(&self) -> String {
        let mut msg = format!(
            "`{}` was removed in v{}; call `{}` with `{}` instead.",
            self.name, self.removed_in, self.replacement, self.replacement_args
        );
        if !self.note.is_empty() {
            msg.push(' ');
            msg.push_str(self.note);
        }
        msg
    }

    /// The replacement as one line for operators, e.g. ``wiki_read` with `scope="shared"``.
    pub fn suggestion(&self) -> String {
        format!("`{}` with `{}`", self.replacement, self.replacement_args)
    }
}

/// The single table of removed MCP tool names. It drives the error a caller
/// of a removed name receives and the `duduclaw doctor` scan for leftover
/// names in employee settings, prompts and skills.
///
/// `shared_wiki_delete` and `wiki_share` were never aliases and stay; neither
/// do `goals_create` (goal hierarchy) or `create_task` (multi-step plans).
pub const REMOVED_MCP_TOOLS: &[RemovedMcpTool] = &[
    RemovedMcpTool {
        name: "shared_wiki_ls",
        removed_in: "1.69.0",
        replacement: "wiki_ls",
        replacement_args: "scope=\"shared\"",
        note: "",
    },
    RemovedMcpTool {
        name: "shared_wiki_read",
        removed_in: "1.69.0",
        replacement: "wiki_read",
        replacement_args: "scope=\"shared\"",
        note: "",
    },
    RemovedMcpTool {
        name: "shared_wiki_write",
        removed_in: "1.69.0",
        replacement: "wiki_write",
        replacement_args: "scope=\"shared\"",
        note: "",
    },
    RemovedMcpTool {
        name: "shared_wiki_search",
        removed_in: "1.69.0",
        replacement: "wiki_search",
        replacement_args: "scope=\"shared\"",
        note: "",
    },
    RemovedMcpTool {
        name: "shared_wiki_stats",
        removed_in: "1.69.0",
        replacement: "wiki_stats",
        replacement_args: "scope=\"shared\"",
        note: "",
    },
    RemovedMcpTool {
        name: "shared_wiki_lint",
        removed_in: "1.69.0",
        replacement: "wiki_lint",
        replacement_args: "scope=\"shared\"",
        note: "",
    },
    RemovedMcpTool {
        name: "schedule_task",
        removed_in: "1.69.0",
        replacement: "tasks_create",
        replacement_args: "schedule=\"<cron expression>\"",
        note: "Pass the old `name` as `title`, `task` as `description`, `agent_id` as \
               `assigned_to`; `notify_channel`, `notify_chat_id`, `notify_thread_id` and \
               `cron_timezone` keep their names.",
    },
    RemovedMcpTool {
        name: "skill_bank_search",
        removed_in: "1.69.0",
        replacement: "skill_search",
        replacement_args: "source=\"bank\"",
        note: "",
    },
];

/// The removed-tool row for `name`, exact match only. Accepts the bare name
/// or the Claude CLI form `mcp__duduclaw__<name>`.
pub fn removed_mcp_tool(name: &str) -> Option<&'static RemovedMcpTool> {
    let bare = name
        .strip_prefix("mcp__")
        .and_then(|rest| rest.strip_prefix(DUDUCLAW_MCP_SERVER))
        .and_then(|rest| rest.strip_prefix("__"))
        .unwrap_or(name);
    REMOVED_MCP_TOOLS.iter().find(|t| t.name == bare)
}

/// The removed name a call is equivalent to, if any: `wiki_<x>` with
/// `scope="shared"` is the former `shared_wiki_<x>`, `tasks_create` with a
/// cron `schedule` the former `schedule_task`, `skill_search` with
/// `source="bank"` the former `skill_bank_search`.
///
/// Restrictive per-agent lists written before the removal (`denied_tools`)
/// still name the old tool; a gate checks this name too so such an entry
/// keeps refusing the call it used to refuse instead of silently lapsing.
/// Never used to *grant* anything.
pub fn removed_name_for_call(tool_name: &str, args: &serde_json::Value) -> Option<&'static str> {
    let arg = |key: &str| args.get(key).and_then(|v| v.as_str()).map(str::trim);
    let arg_is = |key: &str, tokens: &[&str]| {
        arg(key).is_some_and(|v| tokens.iter().any(|t| v.eq_ignore_ascii_case(t)))
    };
    match tool_name {
        "wiki_ls" | "wiki_read" | "wiki_write" | "wiki_search" | "wiki_stats" | "wiki_lint" => {
            if !arg_is("scope", &["shared"]) {
                return None;
            }
            let legacy = match tool_name {
                "wiki_ls" => "shared_wiki_ls",
                "wiki_read" => "shared_wiki_read",
                "wiki_write" => "shared_wiki_write",
                "wiki_search" => "shared_wiki_search",
                "wiki_stats" => "shared_wiki_stats",
                _ => "shared_wiki_lint",
            };
            Some(legacy)
        }
        // An RFC3339 instant goes to the reminder rail, which `schedule_task`
        // never reached; only a cron expression is the old call.
        "tasks_create" => {
            let schedule = arg("schedule")?;
            let is_cron =
                !schedule.is_empty() && chrono::DateTime::parse_from_rfc3339(schedule).is_err();
            is_cron.then_some("schedule_task")
        }
        "skill_search" => arg_is("source", &["bank", "skill_bank"]).then_some("skill_bank_search"),
        _ => None,
    }
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
        "working_state_get",
        "Read the agent's authoritative cross-wake working state",
        "memory:read",
        "memory",
    ),
    (
        "working_state_set",
        "Set one key in the authoritative cross-wake working state",
        "memory:write",
        "memory",
    ),
    (
        "working_state_clear",
        "Retire one key from the cross-wake working state",
        "memory:write",
        "memory",
    ),
    (
        "working_state_handoff",
        "Overwrite the next-wake handoff note",
        "memory:write",
        "memory",
    ),
    (
        "team_handoff",
        "File a TaskPacket for the next role in the team",
        // Internal-only scope (2026-09-28): the packet directory is shared
        // across tasks, so this tool must never be reachable by an external
        // MCP key even though the other `memory` tools are `memory:write`.
        "team:handoff",
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
    // ── Read-only SQL data sources (db:read, WP-D §13.7) ─────────────────
    // One read scope for all four: the connector has no write surface. The
    // dispatch gate ADDITIONALLY requires `[capabilities] db_sources` to name
    // the source, so scope alone never reaches a customer database.
    (
        "db_sources",
        "List the SQL data sources this agent may query",
        "db:read",
        "db",
    ),
    (
        "db_tables",
        "List a data source's tables and columns",
        "db:read",
        "db",
    ),
    (
        "db_select",
        "Read rows from one allowed table (structured, parameterized)",
        "db:read",
        "db",
    ),
    (
        "db_query",
        "Run a read-only SELECT/WITH query against a data source",
        "db:read",
        "db",
    ),
    // ── Local data files (files:read, WP-F2 §14.2) ───────────────────────
    // The ONLY de-identified route to a local CSV / spreadsheet / text file.
    // No per-agent grant: the path fence (agent dir + attachments +
    // `[files] allowed_roots`) is what bounds these, so they are always listed.
    (
        "file_read",
        "Read a plain-text file (txt/md/json/log/yaml)",
        "files:read",
        "files",
    ),
    (
        "csv_read",
        "Read a CSV/TSV file as columns and rows",
        "files:read",
        "files",
    ),
    (
        "xlsx_read",
        "Read one worksheet of an Excel/ODS workbook as columns and rows",
        "files:read",
        "files",
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
    // Forms + Google Tasks reads. Google ships no MCP server for either
    // service, so DuDuClaw serves them natively. `gtasks_*` is Google Tasks —
    // distinct from the internal task board's `tasks_*` tools.
    (
        "forms_get",
        "Read a Google Form's questions",
        "google:read",
        "google",
    ),
    (
        "forms_list_responses",
        "List a Google Form's responses",
        "google:read",
        "google",
    ),
    (
        "gtasks_lists",
        "List Google Tasks task lists",
        "google:read",
        "google",
    ),
    (
        "gtasks_list",
        "List tasks in a Google Tasks list",
        "google:read",
        "google",
    ),
    // Drive / Docs / Slides reads. Google's official MCP servers for these are
    // Developer-Preview-only (terms forbid Pre-GA use outside your domain), so
    // DuDuClaw ships native GA-API tools instead.
    (
        "drive_search",
        "Search Google Drive by name/content",
        "google:read",
        "google",
    ),
    (
        "drive_read",
        "Read a Drive file as text",
        "google:read",
        "google",
    ),
    (
        "docs_read",
        "Read a Google Doc's text",
        "google:read",
        "google",
    ),
    (
        "slides_read",
        "Read a Google Slides deck's text",
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
    (
        "gtasks_create",
        "Create a Google Tasks task",
        "google:write",
        "google",
    ),
    (
        "gtasks_complete",
        "Mark a Google Tasks task complete",
        "google:write",
        "google",
    ),
    (
        "docs_append",
        "Append text to a Google Doc",
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
    // ── O-0 system-operator tool face (admin) ─────────────────────────────
    // DESIGN-agent-os-native-apps-2026-08.md §6.3 O-0: bridges the
    // dashboard-only device.*/system.* RPCs to agents. Admin-scoped — a
    // strictly higher trust tier than `os:native`'s host-automation tools,
    // matching `mcp_auth::tool_requires_scope`'s explicit mapping.
    (
        "os_device_status",
        "Appliance CPU/RAM/disk/temperature/uptime/network snapshot",
        "admin",
        "os",
    ),
    (
        "os_system_status",
        "Reduced system status (version/agent count/edition/channels)",
        "admin",
        "os",
    ),
    (
        "os_check_update",
        "Check for a duduclaw self-update and/or appliance OS image update",
        "admin",
        "os",
    ),
    (
        "os_backup_list",
        "List device backups under <home>/backups/",
        "admin",
        "os",
    ),
    (
        "os_network_info",
        "Appliance network interfaces",
        "admin",
        "os",
    ),
    (
        "os_wifi_status",
        "Wi-Fi link state, IP info, and internet/captive-portal connectivity (agent-body network slice)",
        "admin",
        "os",
    ),
    (
        "os_wifi_scan",
        "Scan for nearby Wi-Fi networks: SSID/signal/security/known (agent-body network slice)",
        "admin",
        "os",
    ),
    (
        "os_wifi_connect",
        "Join a Wi-Fi network by SSID, no psk param (open/already-known credential only, destructive, confirm required)",
        "admin",
        "os",
    ),
    (
        "os_apply_update",
        "Apply an update (device OS image or duduclaw self-update, destructive, confirm required)",
        "admin",
        "os",
    ),
    (
        "os_boot_assessment",
        "Read systemd's automatic boot assessment for the running version (agent-body update slice)",
        "admin",
        "os",
    ),
    (
        "os_update_rollback",
        "Roll back to the previous A/B slot and reboot (destructive, confirm required, agent-body update slice)",
        "admin",
        "os",
    ),
    (
        "os_backup_create",
        "Archive the device's writable data partition",
        "admin",
        "os",
    ),
    (
        "os_power",
        "Restart or shut down the appliance (destructive, confirm required)",
        "admin",
        "os",
    ),
    (
        "os_factory_reset",
        "Wipe device state and re-provision (irreversible, confirm + approval required)",
        "admin",
        "os",
    ),
    (
        "os_doctor_repair",
        "Reduced health checks with repair hints",
        "admin",
        "os",
    ),
    (
        "os_display_get",
        "Read display appearance: cursor size/source, comp's theme, primary screen scale (A7c agent→display bridge)",
        "admin",
        "os",
    ),
    (
        "os_display_set",
        "Change one display appearance field live (cursor_size/cursor_source/theme/output_scale — A7c \"make text bigger\" backend)",
        "admin",
        "os",
    ),
    (
        "os_audio_get",
        "Read current audio state: volume percentage, mute, and every output device (Y10-1 agent→audio bridge)",
        "admin",
        "os",
    ),
    (
        "os_audio_set",
        "Change one audio field live (volume 0-100 / mute toggle / output device id — Y10-1 \"turn it up / mute\" backend)",
        "admin",
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
        "Create task/goal (Admin), or discovery (discovery:execute)",
        "admin",
        "task",
    ),
    ("discovery_catalog", "List approved discovery roots, evaluators and runtimes", "discovery:execute", "discovery"),
    ("discovery_list", "List authorized discovery runs", "discovery:execute", "discovery"),
    ("discovery_tree", "Read a discovery tree and durable accounting", "discovery:execute", "discovery"),
    ("discovery_artifact", "Read a verified discovery artifact", "discovery:execute", "discovery"),
    ("discovery_cancel", "Cancel an authorized discovery run", "discovery:execute", "discovery"),
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
    // ── Market Belief Loop (design-market-belief-loop-2026-08.md) ─────────
    // Unmapped in `tool_requires_scope` (same C2 fail-closed default as the
    // task-board family above), so scope is "admin" here too.
    (
        "belief_submit",
        "Record a structured belief about an external subject",
        "admin",
        "prediction",
    ),
    (
        "belief_settle",
        "Settle a belief against a realized outcome",
        "admin",
        "prediction",
    ),
    (
        "belief_stats",
        "Read the agent's own belief calibration track record",
        "admin",
        "prediction",
    ),
    // ── Agent Mail (P2-d) ────────────────────────────────────────────────
    // Read and draft-send are separate grants, matching
    // `tool_requires_scope`: an operator can let an AI employee see the
    // mailbox without letting it queue outbound correspondence. `mail_send`
    // cannot transmit — a human confirmation does — but it is the tool that
    // puts a message in front of a person, so it is gated on its own.
    (
        "mail_list",
        "List mail that arrived in the agent's mailbox",
        "mail:read",
        "channel",
    ),
    (
        "mail_read",
        "Read one message in full and mark it read",
        "mail:read",
        "channel",
    ),
    (
        "mail_send",
        "Draft an outgoing email for human confirmation (never sends)",
        "mail:send",
        "channel",
    ),
    // ── Human-machine co-drive (CD-1, DESIGN-codrive-desktop-2026-08.md) ──
    // GUI-level mouse/keyboard injection into a shared desktop, gated by
    // `[capabilities] codrive` (deny-by-default) on top of Admin scope —
    // matching `tool_requires_scope`'s explicit `codrive_run` arm.
    (
        "codrive_run",
        "Run a scripted human-machine co-drive session (GUI mouse/keyboard, human-supervised)",
        "admin",
        "codrive",
    ),
    (
        "codrive_status",
        "Read who is driving the shared desktop right now (read-only)",
        "admin",
        "codrive",
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
    (
        "computer_navigate",
        "Open an allowlisted https page",
        "admin",
        "computer",
    ),
    (
        "computer_workspace_list",
        "List your durable computer-use workspaces",
        "admin",
        "computer",
    ),
    (
        "computer_workspace_read",
        "Read a text file from your workspace",
        "admin",
        "computer",
    ),
    (
        "computer_workspace_write",
        "Write a text file into your attached workspace",
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
            deprecated: is_deprecated_tool(name),
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
            deprecated: false,
        });
    }
    out
}

// ── LLM-loop tools (not MCP) ─────────────────────────────────────────────

/// The two reversible-context-retrieval tool names, exposed as constants so
/// producer and consumer compare against the same string.
pub const CCR_RETRIEVE_TOOL: &str = "duduclaw_ccr_retrieve";
/// See [`CCR_RETRIEVE_TOOL`].
pub const CCR_FIND_TOOL: &str = "duduclaw_ccr_find";

/// A tool the **direct-API tool loop** injects into a `ChatRequest`, as opposed
/// to one the MCP server dispatches.
///
/// Deliberately a separate shape from [`ToolCatalogEntry`]: these tools carry a
/// real JSON Schema (the MCP table's every parameter is a bare string), they
/// have no `mcp_auth` scope because no MCP dispatch gate ever sees them, and
/// they must not show up in the dashboard capability picker — an operator
/// cannot grant or deny them, the CCR runtime's own scope binding decides
/// access.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LlmToolSchema {
    pub name: &'static str,
    pub description: &'static str,
    pub input_schema: serde_json::Value,
}

/// The CCR tool schemas — **the single registration authority** (O7).
///
/// These used to be two `ToolDef` literals inlined in
/// `duduclaw_llm::tool_loop::loop_run`, a second registration face living far
/// from every other tool definition in the project: nothing linked it to the
/// catalog, nothing held it to the description budget, and a caller wanting the
/// schema had to copy it. They live here instead, in the crate both
/// `duduclaw-llm` (the tool loop that injects them) and `duduclaw-cli` (the
/// `ccr run` harness and the registration-drift tests) can reach.
///
/// Descriptions are held to the same 200-byte O7 budget as the MCP table; the
/// full behaviour is documented in `docs/spec/reversible-context-ccr.md`.
pub fn ccr_tool_schemas() -> Vec<LlmToolSchema> {
    vec![
        LlmToolSchema {
            name: CCR_RETRIEVE_TOOL,
            description: "Retrieve an authorized, original tool-result fragment by CCR ID. \
                          Use query to find a specific passage; results are bounded.",
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "id": {"type": "string"},
                    "query": {"type": "string"},
                    "offset": {"type": "integer", "minimum": 0},
                    "limit": {"type": "integer", "minimum": 1, "maximum": 65536}
                },
                "required": ["id"]
            }),
        },
        LlmToolSchema {
            name: CCR_FIND_TOOL,
            // The byte range stays in the prose on purpose: it is a runtime
            // rule, not a schema keyword. `minLength`/`maxLength` would be
            // counted in UTF-16 code units by a provider's validator and would
            // reject valid CJK queries, so the model is told the rule instead
            // (pinned by `tool_loop::tests::cases_loop`).
            description: "Find up to five CCR handles from earlier turns in this session. \
                          Query is 3–128 UTF-8 bytes. A hit says whether the phrase matched \
                          exactly; a lexical hit must be retrieved by id and offset.",
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "3–128 UTF-8 bytes; the runtime enforces the byte limit"
                    },
                    "limit": {"type": "integer", "minimum": 1, "maximum": 5}
                },
                "required": ["query"]
            }),
        },
    ]
}

/// Strip an optional `(qualifier)` suffix and an optional `mcp__<server>__`
/// namespace prefix from a `[capabilities] allowed_tools` / `denied_tools`
/// entry, returning the bare tool name a security gate can compare against.
///
/// Both spellings must resolve to the same identity so an operator-authored
/// entry (bare, e.g. `memory_search`) and a dashboard-authored entry
/// (qualified, e.g. `mcp__duduclaw__memory_search`) enforce identically
/// regardless of which layer reads it — the Claude CLI's `--disallowedTools`
/// (bare-or-qualified, see `duduclaw-gateway::claude_runner::tool_base_name`,
/// the original implementation this mirrors) and the MCP dispatch gate
/// (`duduclaw-cli::mcp_dispatch`, WP-H2 §1.3 Gap (b) — the MCP transport only
/// ever sees bare names, since that's what JSON-RPC `tools/call` carries).
///
/// A degenerate qualifier with an empty tool segment (`"mcp__duduclaw__"`)
/// deliberately does NOT strip to `""` — that would make an operator typo
/// silently deny/allow *every* MCP tool. Comparison callers should use exact
/// equality on the returned string (project coding convention 2: no
/// unanchored substring matching for security decisions).
pub fn mcp_tool_base_name(entry: &str) -> &str {
    let e = entry.split('(').next().unwrap_or(entry).trim();
    if let Some(rest) = e.strip_prefix("mcp__") {
        if let Some(idx) = rest.find("__") {
            let bare = &rest[idx + 2..];
            if !bare.is_empty() {
                return bare;
            }
        }
    }
    e
}

/// The MCP server name DuDuClaw's own tools are served under
/// (`mcp__duduclaw__<tool>` in Claude CLI permission rules).
pub const DUDUCLAW_MCP_SERVER: &str = "duduclaw";

/// Split a tool reference into `(server, name)`. A bare name belongs to the
/// duduclaw server; `mcp__<server>__<name>` names `<server>`; a bare
/// `mcp__<server>` (Claude CLI's server-level rule) is `(server, "*")`. An
/// optional `(qualifier)` suffix is dropped.
fn split_tool_ref(raw: &str) -> (&str, &str) {
    let e = raw.split('(').next().unwrap_or(raw).trim();
    match e.strip_prefix("mcp__") {
        Some(rest) => match rest.find("__") {
            Some(idx) => (&rest[..idx], &rest[idx + 2..]),
            None => (rest, "*"),
        },
        None => (DUDUCLAW_MCP_SERVER, e),
    }
}

/// Name part of a list entry against a tool name: exact, or — only when the
/// entry ends in `*` and has no other `*` — an anchored prefix match (`*`
/// alone matches every name). A `*` anywhere else is literal, so it only
/// matches a tool literally named that way (none exists).
fn name_pattern_matches(pattern: &str, name: &str) -> bool {
    if pattern.is_empty() || name.is_empty() {
        return false;
    }
    if let Some(prefix) = pattern.strip_suffix('*') {
        if !prefix.contains('*') {
            return name.starts_with(prefix);
        }
    }
    pattern == name
}

/// Does one `[capabilities]` list entry (`allowed_tools`, `denied_tools`,
/// `scoped_tools`, `approval_required_tools`, `irreversible_tools`,
/// `maybe_irreversible_tools`) name `tool`?
///
/// The Claude CLI rule semantics, anchored — never a substring match:
/// - `*` matches every tool;
/// - `mcp__duduclaw__*` (or `mcp__duduclaw`) matches every duduclaw tool;
/// - `mcp__duduclaw__<prefix>*` / bare `<prefix>*` match duduclaw tools whose
///   name starts with `<prefix>` (`*` only at the end);
/// - `mcp__duduclaw__<name>` / bare `<name>` match exactly that tool;
/// - `mcp__<other-server>__…` never matches a duduclaw tool (it names
///   another server), and vice versa.
///
/// `tool` is what the gate sees: a bare duduclaw name, or a qualified name.
pub fn tool_entry_matches(entry: &str, tool: &str) -> bool {
    if entry.trim() == "*" {
        return true;
    }
    let (entry_server, entry_name) = split_tool_ref(entry);
    let (tool_server, tool_name) = split_tool_ref(tool);
    entry_server == tool_server && name_pattern_matches(entry_name, tool_name)
}

/// True when any entry of `list` names `tool` (see [`tool_entry_matches`]).
pub fn tool_list_matches<S: AsRef<str>>(list: impl IntoIterator<Item = S>, tool: &str) -> bool {
    list.into_iter().any(|e| tool_entry_matches(e.as_ref(), tool))
}

/// What `[capabilities] denied_tools` / `allowed_tools` decide for one tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolListVerdict {
    /// Neither list stops the call.
    Allowed,
    /// The tool is in `denied_tools` (always wins over `allowed_tools`).
    Denied,
    /// `allowed_tools` is non-empty (allowlist mode) and does not name it.
    NotAllowlisted,
}

/// The `denied_tools` / `allowed_tools` decision shared by every gate that
/// enforces them (the MCP dispatch front door, the `tools/list` filter and
/// the gateway's computer-use route), using [`tool_entry_matches`] (Claude
/// CLI wildcard rules, anchored, server-aware); `denied_tools` wins; a
/// non-empty `allowed_tools` switches the agent into allowlist mode.
///
/// v1.68.1: before this, entries were compared by exact base name, so the
/// documented `mcp__duduclaw__*` allowlist entry reduced to `*` and matched
/// nothing — every platform tool was refused for such employees — and
/// `mcp__<other>__foo` wrongly matched a duduclaw tool named `foo`.
pub fn tool_list_verdict(tool_name: &str, denied: &[String], allowed: &[String]) -> ToolListVerdict {
    if tool_list_matches(denied, tool_name) {
        return ToolListVerdict::Denied;
    }
    if !allowed.is_empty() && !tool_list_matches(allowed, tool_name) {
        return ToolListVerdict::NotAllowlisted;
    }
    ToolListVerdict::Allowed
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn strings(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn production_wildcard_allowlist_allows_platform_tools() {
        let allowed = strings(&[
            "mcp__duduclaw__*", "mcp__masterlink__*", "WebSearch", "WebFetch", "Read", "Write", "Edit", "Glob",
            "Grep", "TodoWrite",
        ]);
        for tool in ["memory_store", "user_profile_get", "working_state_set"] {
            assert_eq!(tool_list_verdict(tool, &[], &allowed), ToolListVerdict::Allowed, "{tool}");
        }
        let other_only = strings(&["mcp__masterlink__*"]);
        assert_eq!(tool_list_verdict("memory_store", &[], &other_only), ToolListVerdict::NotAllowlisted);
    }

    #[test]
    fn wildcard_rules_are_anchored_and_server_aware() {
        let denied = strings(&["mcp__duduclaw__odoo_*"]);
        assert_eq!(tool_list_verdict("odoo_search", &denied, &[]), ToolListVerdict::Denied);
        assert_eq!(tool_list_verdict("memory_search", &denied, &[]), ToolListVerdict::Allowed);
        // Prefix is anchored at the start.
        assert!(tool_entry_matches("memory_*", "memory_store"));
        assert!(!tool_entry_matches("memory_*", "agent_memory_x"));
        // `*` anywhere but the end is literal.
        assert!(!tool_entry_matches("mem*ry_store", "memory_store"));
        assert!(!tool_entry_matches("*_store", "memory_store"));
        // Lone `*` and the server-level rule.
        assert!(tool_entry_matches("*", "memory_store"));
        assert!(tool_entry_matches("mcp__duduclaw", "memory_store"));
        // Another server never names a duduclaw tool.
        assert!(!tool_entry_matches("mcp__masterlink__foo", "foo"));
        assert!(!tool_entry_matches("mcp__masterlink__*", "memory_store"));
        assert_eq!(tool_list_verdict("foo", &strings(&["mcp__masterlink__foo"]), &[]), ToolListVerdict::Allowed);
        // Qualified tool names on both sides.
        assert!(tool_entry_matches("memory_store", "mcp__duduclaw__memory_store"));
        assert!(!tool_entry_matches("mcp__duduclaw__", "memory_store"));
    }

    #[test]
    fn tool_list_verdict_denied_wins_and_allowlist_is_exact() {
        let v = |d: &[&str], a: &[&str]| {
            let d: Vec<String> = d.iter().map(|s| s.to_string()).collect();
            let a: Vec<String> = a.iter().map(|s| s.to_string()).collect();
            tool_list_verdict("computer_click", &d, &a)
        };
        assert_eq!(v(&[], &[]), ToolListVerdict::Allowed);
        assert_eq!(v(&["mcp__duduclaw__computer_click"], &["computer_click"]), ToolListVerdict::Denied);
        assert_eq!(v(&[], &["computer_clicker"]), ToolListVerdict::NotAllowlisted);
        assert_eq!(v(&[], &["computer"]), ToolListVerdict::NotAllowlisted);
        assert_eq!(v(&["computer"], &[]), ToolListVerdict::Allowed);
        assert_eq!(v(&[], &["mcp__duduclaw__computer_click"]), ToolListVerdict::Allowed);
    }

    #[test]
    fn test_mcp_tool_base_name_bare_passthrough() {
        assert_eq!(mcp_tool_base_name("memory_search"), "memory_search");
    }

    #[test]
    fn test_mcp_tool_base_name_strips_qualifier() {
        assert_eq!(
            mcp_tool_base_name("mcp__duduclaw__memory_search"),
            "memory_search"
        );
    }

    #[test]
    fn test_mcp_tool_base_name_strips_paren_suffix() {
        assert_eq!(mcp_tool_base_name("Bash(git:*)"), "Bash");
    }

    #[test]
    fn test_mcp_tool_base_name_degenerate_prefix_does_not_collapse_to_empty() {
        // A typo'd qualifier with no tool segment must not become a wildcard.
        assert_eq!(mcp_tool_base_name("mcp__duduclaw__"), "mcp__duduclaw__");
    }

    #[test]
    fn test_mcp_tool_base_name_cross_form_equality() {
        assert_eq!(
            mcp_tool_base_name("mcp__duduclaw__office_script"),
            mcp_tool_base_name("office_script")
        );
    }

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

    /// v1.69.0: a removed name is gone from the catalog (the dashboard picker
    /// must not offer it), and the tool that replaces it is still there.
    #[test]
    fn removed_tools_are_out_of_the_catalog_and_their_replacements_are_in() {
        let catalog = builtin_tool_catalog();
        assert_eq!(REMOVED_MCP_TOOLS.len(), 8);
        for removed in REMOVED_MCP_TOOLS {
            assert!(
                !catalog.iter().any(|e| e.name == removed.name),
                "{} was removed and must not be catalogued",
                removed.name
            );
            let replacement = catalog
                .iter()
                .find(|e| e.name == removed.replacement)
                .unwrap_or_else(|| panic!("{} must be catalogued", removed.replacement));
            assert!(!replacement.deprecated, "{} is not an alias", removed.replacement);
        }
        // The two shared-wiki tools that were never aliases stay.
        for kept in ["shared_wiki_delete", "wiki_share"] {
            assert!(catalog.iter().any(|e| e.name == kept), "{kept}");
            assert!(removed_mcp_tool(kept).is_none(), "{kept}");
        }
    }

    #[test]
    fn removed_tool_message_names_the_replacement_and_its_argument() {
        let row = removed_mcp_tool("shared_wiki_read").unwrap();
        assert_eq!(
            row.message(),
            "`shared_wiki_read` was removed in v1.69.0; call `wiki_read` with `scope=\"shared\"` instead."
        );
        let row = removed_mcp_tool("schedule_task").unwrap();
        assert!(row.message().contains("`tasks_create` with `schedule=\"<cron expression>\"`"));
        assert!(row.message().contains("`assigned_to`"));
        assert_eq!(
            removed_mcp_tool("skill_bank_search").unwrap().suggestion(),
            "`skill_search` with `source=\"bank\"`"
        );
    }

    #[test]
    fn removed_tool_lookup_is_exact_and_accepts_the_qualified_form() {
        assert!(removed_mcp_tool("mcp__duduclaw__shared_wiki_write").is_some());
        assert!(removed_mcp_tool("shared_wiki_write_v2").is_none());
        assert!(removed_mcp_tool("schedule_tasks").is_none());
        assert!(removed_mcp_tool("mcp__other__shared_wiki_write").is_none());
        assert!(removed_mcp_tool("mcp__duduclawx__shared_wiki_write").is_none());
        assert!(!is_deprecated_tool("shared_wiki_write"));
    }

    #[test]
    fn removed_name_for_call_maps_only_the_equivalent_arguments() {
        use serde_json::json;
        assert_eq!(
            removed_name_for_call("wiki_write", &json!({"scope": " Shared "})),
            Some("shared_wiki_write")
        );
        assert_eq!(removed_name_for_call("wiki_read", &json!({})), None);
        assert_eq!(removed_name_for_call("wiki_read", &json!({"scope": "agent"})), None);
        assert_eq!(
            removed_name_for_call("tasks_create", &json!({"schedule": "0 9 * * *"})),
            Some("schedule_task")
        );
        assert_eq!(
            removed_name_for_call("tasks_create", &json!({"schedule": "2026-10-01T09:00:00+08:00"})),
            None
        );
        assert_eq!(removed_name_for_call("tasks_create", &json!({"title": "x"})), None);
        assert_eq!(
            removed_name_for_call("skill_search", &json!({"source": "bank"})),
            Some("skill_bank_search")
        );
        assert_eq!(removed_name_for_call("skill_search", &json!({})), None);
        assert_eq!(removed_name_for_call("shared_wiki_delete", &json!({})), None);
    }

    // ── O7: CCR is a registration face, not an MCP tool ──────────────────

    /// The schemas must be usable as-is: a name, non-empty prose within the
    /// same 200-byte budget the MCP table carries, and a real object schema.
    #[test]
    fn ccr_tool_schemas_are_well_formed_and_within_the_description_budget() {
        let schemas = ccr_tool_schemas();
        assert_eq!(schemas.len(), 2);
        for s in &schemas {
            assert!(!s.name.is_empty());
            assert!(!s.description.is_empty());
            assert!(
                s.description.len() <= 200,
                "{} description is {} bytes, over the 200-byte O7 budget",
                s.name,
                s.description.len()
            );
            assert_eq!(s.input_schema["type"], "object");
            assert!(s.input_schema["properties"].is_object());
            assert!(s.input_schema["required"].is_array());
        }
        let names: Vec<_> = schemas.iter().map(|s| s.name).collect();
        assert!(names.contains(&CCR_RETRIEVE_TOOL));
        assert!(names.contains(&CCR_FIND_TOOL));
    }

    /// CCR tools are injected by the direct-API tool loop and are NOT
    /// dispatched by the MCP server, so they must never leak into the MCP
    /// catalog — where they would demand an `mcp_auth` scope arm, appear in
    /// the dashboard capability picker, and be advertised by `tools/list` as
    /// something the MCP dispatcher could run. They cannot.
    #[test]
    fn ccr_tools_are_not_in_the_mcp_catalog() {
        let catalog = builtin_tool_catalog();
        for name in [CCR_RETRIEVE_TOOL, CCR_FIND_TOOL] {
            assert!(
                !catalog.iter().any(|e| e.name == name),
                "{name} must not be in the MCP/Claude capability catalog"
            );
        }
    }
}
