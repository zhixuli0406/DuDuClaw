//! The MCP scope vocabulary and the tool → minimum-scope table.
//!
//! Split out of the single 2,110-line `mcp_auth.rs` on 2026-09-29 (audit
//! O14). The [`Scope`] variants, their wire strings, and every row of
//! [`tool_requires_scope`] are moved **verbatim** — `scope_enum_matches_
//! canonical_list` and `test_catalog_scopes_match_tool_requires_scope` are
//! the guards that would catch any drift.

use std::collections::HashSet;

use super::AuthError;


#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Scope {
    MemoryRead,
    MemoryWrite,
    WikiRead,
    WikiWrite,
    MessagingSend,
    /// RFC-21 §1: gates `identity_resolve` and friends. Distinct from
    /// `WikiRead` because operators may want to grant agents read access to
    /// the shared wiki *without* exposing the canonical person registry.
    IdentityRead,
    /// RFC-21 §2: gates Odoo `search_read` / list / status — read-class
    /// `odoo_*` MCP tools that don't mutate Odoo state.
    OdooRead,
    /// RFC-21 §2: gates Odoo `create` / `write` — mutating `odoo_*` tools
    /// that change record state but don't fire workflows.
    OdooWrite,
    /// RFC-21 §2: gates Odoo `execute_kw` workflow buttons (e.g.
    /// `action_confirm`) and the generic `odoo_execute` / `odoo_report`
    /// surfaces, which can fire side-effects beyond simple writes.
    OdooExecute,
    /// Google Workspace: gates the read-class native tools (`google_status`,
    /// `gmail_search`, `gmail_read`, `calendar_list_events`). Distinct scope so
    /// operators can grant Gmail/Calendar read without full `Admin`.
    GoogleRead,
    /// Google Workspace: gates the write-class native tools
    /// (`gmail_create_draft` — draft only, never sends; `calendar_create_event`
    /// — creates a real, externally-visible event; `sheets_append`).
    GoogleWrite,
    /// Notion: gates the read-class native tools (`notion_status`,
    /// `notion_search`, `notion_page_read`). Notion content is an external
    /// knowledge source surfaced for query/citation only.
    NotionRead,
    /// Notion: gates the write-class native tool (`notion_page_append` — appends
    /// paragraph blocks to an existing page; never deletes/overwrites).
    NotionWrite,
    /// GitHub: gates the read-class native tools (`github_status`,
    /// `github_search_issues`, `github_issue_read`, `github_pr_read`).
    GithubRead,
    /// GitHub: gates the write-class native tool (`github_issue_comment` — posts
    /// a publicly visible comment).
    GithubWrite,
    /// RFC-26: gates the Live Run Forking tools (`fork_run`, `inspect_branches`,
    /// `diff_branches`, `merge_or_select`, `terminate_branch`, `fork_cost`).
    /// Distinct from `Admin` so operators can grant an agent the ability to fork
    /// its own runs without granting full superuser scope.
    ForkExecute,
    /// OS-native Phase 1: gates the `os_notify` / `os_watch_status` / `os_open`
    /// MCP tools. Distinct scope so operators can grant OS integration without
    /// granting `Admin`; enforcement additionally requires the per-agent
    /// `[capabilities] os_native` flag at the dispatch gate (defence-in-depth).
    OsNative,
    /// Gates the `office_script` MCP tool — server-side execution of a bundled
    /// office skill's vetted `scripts/*.py` (docx/xlsx/pptx/pdf) so API-mode
    /// agents that have no Bash tool can still produce document files.
    /// Deliberately narrower than `Admin` (which the code-execution
    /// `execute_program` requires): constrained to the four built-in skills and
    /// the caller's own agent directory, so operators can grant document
    /// production without granting superuser.
    SkillExecute,
    /// WP3.3 recording-to-skill: gates the `browser_record_start` /
    /// `browser_record_stop` / `desktop_record_start` / `desktop_record_stop` /
    /// `skill_from_recording` MCP tools. Distinct scope so operators can grant
    /// recording without Admin; the dispatch gate ADDITIONALLY requires the
    /// per-agent `[capabilities] recording = true` flag (defence-in-depth,
    /// deny-by-default).
    Recording,
    /// Agent Mail (P2-d): gates `mail_list` / `mail_read`. Split from
    /// [`Scope::MailSend`] so an operator can let an agent *see* the mailbox
    /// without granting it the ability to queue outbound correspondence.
    MailRead,
    /// Agent Mail (P2-d): gates `mail_send`. Named as a send scope even though
    /// the tool cannot transmit — what it grants is the ability to put a draft
    /// in front of a human, which is the step worth authorising separately.
    MailSend,
    /// WP-D (§13.7): gates the read-only SQL connector tools (`db_sources` /
    /// `db_tables` / `db_select` / `db_query`). Its own scope so a customer
    /// database can be granted without `Admin`; the dispatch gate
    /// ADDITIONALLY requires the per-agent `[capabilities] db_sources` list
    /// to name the source (defence-in-depth, deny-by-default). There is no
    /// write counterpart on purpose — the connector cannot write.
    DbRead,
    /// WP-F2 (§14.2): gates the local data-file tools (`file_read` /
    /// `csv_read` / `xlsx_read`). Its own scope — reading a customer's CSV is
    /// not the same authority as reading memory or a wiki page — and
    /// deliberately NOT in [`EXTERNALLY_GRANTABLE_SCOPES`]: a remote MCP
    /// client has no business reading this host's filesystem. Unlike
    /// [`Scope::DbRead`] there is no second per-agent grant, because the path
    /// fence in `mcp_files::vet_path` (the caller's own agent directory,
    /// `<home>/attachments`, and the operator's `[files] allowed_roots`)
    /// already bounds what any caller can reach.
    FilesRead,
    /// Team-as-Agent (`team_handoff`): filing a `TaskPacket` across a role
    /// boundary. Its own scope — and deliberately NOT in
    /// [`EXTERNALLY_GRANTABLE_SCOPES`] — because an unpinned handoff writes
    /// into `<home>/team_packets/<task>/…`, a directory **shared across
    /// tasks**, not the caller's own per-agent state. It used to ride on
    /// [`Scope::MemoryWrite`] "for zero added isolation" alongside
    /// `working_state_*`; the 2026-09-28 review showed the analogy does not
    /// hold (`working_state_*` only ever touches `<agent_dir>/state/`), and
    /// `MemoryWrite` *is* externally grantable, so any external MCP key
    /// holding `memory:write` could reach the team channel.
    TeamHandoff,
    /// Internal discovery requests and ACL-bound queries; never externally grantable.
    DiscoveryExecute,
    Admin,
}

impl std::fmt::Display for Scope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Scope::MemoryRead => "memory:read",
            Scope::MemoryWrite => "memory:write",
            Scope::WikiRead => "wiki:read",
            Scope::WikiWrite => "wiki:write",
            Scope::MessagingSend => "messaging:send",
            Scope::IdentityRead => "identity:read",
            Scope::OdooRead => "odoo:read",
            Scope::OdooWrite => "odoo:write",
            Scope::OdooExecute => "odoo:execute",
            Scope::GoogleRead => "google:read",
            Scope::GoogleWrite => "google:write",
            Scope::NotionRead => "notion:read",
            Scope::NotionWrite => "notion:write",
            Scope::GithubRead => "github:read",
            Scope::GithubWrite => "github:write",
            Scope::ForkExecute => "fork:execute",
            Scope::OsNative => "os:native",
            Scope::SkillExecute => "skill:execute",
            Scope::Recording => "recording",
            Scope::MailRead => "mail:read",
            Scope::MailSend => "mail:send",
            Scope::DbRead => "db:read",
            Scope::FilesRead => "files:read",
            Scope::TeamHandoff => "team:handoff",
            Scope::DiscoveryExecute => "discovery:execute",
            Scope::Admin => "admin",
        };
        write!(f, "{s}")
    }
}

/// Map a single canonical scope wire string to its `Scope` variant. The
/// reverse of `Scope`'s `Display` impl above — kept immediately next to
/// `parse_scopes` so the two stay easy to eyeball together, and locked
/// bidirectionally against `duduclaw_core::mcp_scopes::MCP_SCOPE_STRINGS` (the
/// shared canonical list the gateway also reads) by the
/// `scope_enum_matches_canonical_list` test below.
fn scope_from_str(s: &str) -> Option<Scope> {
    Some(match s {
        "memory:read" => Scope::MemoryRead,
        "memory:write" => Scope::MemoryWrite,
        "wiki:read" => Scope::WikiRead,
        "wiki:write" => Scope::WikiWrite,
        "messaging:send" => Scope::MessagingSend,
        "identity:read" => Scope::IdentityRead,
        "odoo:read" => Scope::OdooRead,
        "odoo:write" => Scope::OdooWrite,
        "odoo:execute" => Scope::OdooExecute,
        "google:read" => Scope::GoogleRead,
        "google:write" => Scope::GoogleWrite,
        "notion:read" => Scope::NotionRead,
        "notion:write" => Scope::NotionWrite,
        "github:read" => Scope::GithubRead,
        "github:write" => Scope::GithubWrite,
        "fork:execute" => Scope::ForkExecute,
        "os:native" => Scope::OsNative,
        "skill:execute" => Scope::SkillExecute,
        "mail:read" => Scope::MailRead,
        "mail:send" => Scope::MailSend,
        "db:read" => Scope::DbRead,
        "files:read" => Scope::FilesRead,
        "team:handoff" => Scope::TeamHandoff,
        "recording" => Scope::Recording,
        "discovery:execute" => Scope::DiscoveryExecute,
        "admin" => Scope::Admin,
        _ => return None,
    })
}

/// Parse a comma-separated scope string into a HashSet<Scope>.
/// e.g. "memory:read,wiki:write" → {MemoryRead, WikiWrite}
pub fn parse_scopes(s: &str) -> Result<HashSet<Scope>, AuthError> {
    if s.trim().is_empty() {
        return Ok(HashSet::new());
    }

    let mut result = HashSet::new();
    for part in s.split(',') {
        let part = part.trim();
        match scope_from_str(part) {
            Some(scope) => {
                result.insert(scope);
            }
            None => return Err(AuthError::InvalidScope(part.to_string())),
        }
    }
    Ok(result)
}

/// Return the minimum Scope required to call this tool.
///
/// C2 (2026-06 deep review): this table is **fail-closed**. Every tool not
/// explicitly mapped to a narrower scope falls through to `Some(Scope::Admin)`,
/// so a deliberately narrow-scoped key (e.g. `memory:read`) can never reach an
/// unenumerated high-impact tool (`execute_program`, `agent_update_soul`, …).
/// The default in-process agent uses `default_internal_principal`, which holds
/// `Scope::Admin` (a superuser in the dispatcher check), so normal operation is
/// unaffected. When adding a new tool, map it to the least scope it needs here;
/// leaving it unmapped means it requires Admin.
pub fn tool_requires_scope(tool_name: &str) -> Option<Scope> {
    match tool_name {
        "discovery_catalog" | "discovery_list" | "discovery_tree" | "discovery_artifact" | "discovery_cancel" => Some(Scope::DiscoveryExecute),
        // ── Memory: read family ──────────────────────────────────────────
        "memory_search"
        | "memory_read"
        | "memory_fetch_batch"
        // D1 bi-temporal read APIs — same read tier as the rest of the family.
        | "memory_get_history"
        | "memory_get_at"
        // D3.2 entity-alias listing — read tier.
        | "memory_alias_list"
        | "memory_search_by_layer"
        | "memory_successful_conversations"
        | "memory_consolidation_status"
        | "memory_improve"
        | "memory_episodic_pressure"
        | "user_profile_get"
        | "user_code_profile"
        // Cross-wake working state — read tier.
        | "working_state_get"
        | "code_map" => Some(Scope::MemoryRead),
        // D3.2 entity-alias mutation — write tier.
        "memory_store" | "memory_alias_add" | "user_profile_record" => Some(Scope::MemoryWrite),
        // Cross-wake working state mutation (D3 ghost-memory fix) — the
        // agent's own authoritative posture, same trust tier as memory_store.
        "working_state_set" | "working_state_clear" | "working_state_handoff" => {
            Some(Scope::MemoryWrite)
        }
        // Team-as-Agent handoff (P1/WP-5). This used to be mapped to
        // `Scope::MemoryWrite` on the argument that filing a TaskPacket is the
        // same trust tier as a working-state write. The 2026-09-28 review
        // retired that argument: `working_state_*` writes ONLY the caller's own
        // `<agent_dir>/state/`, whereas an unpinned `team_handoff` writes into
        // `<home>/team_packets/<task>/r<n>/`, a directory shared across tasks —
        // and `MemoryWrite` is in `EXTERNALLY_GRANTABLE_SCOPES`, so any external
        // MCP key carrying `memory:write` reached the team channel. It now has
        // its own scope, which is deliberately NOT externally grantable.
        // Internal callers are unaffected: the dispatch gate accepts
        // `Scope::Admin` as a substitute for any required scope, and every
        // gateway-spawned MCP child authenticates with the `admin`-scoped
        // `gateway-internal` key (see `mcp_internal_key::internal_key_entry`).
        "team_handoff" => Some(Scope::TeamHandoff),
        // ── Wiki: read family ────────────────────────────────────────────
        "wiki_read"
        | "wiki_search"
        | "wiki_ls"
        | "wiki_stats"
        | "wiki_export"
        | "wiki_graph"
        | "wiki_lint"
        | "wiki_namespace_status" => Some(Scope::WikiRead),
        // ── Wiki: write family (incl. destructive shared_wiki_delete) ─────
        "wiki_write"
        | "wiki_share"
        | "wiki_dedup"
        | "wiki_rebuild_fts"
        | "shared_wiki_delete" => Some(Scope::WikiWrite),
        // ── G15 Live Canvas ──────────────────────────────────────────────
        // Agent-authored presentation content pushed to the dashboard — same
        // trust tier as wiki_write (agent-visible content mutation;
        // server-side ammonia-sanitized at write, sandbox-iframed at render).
        // No MCP read tool exists: viewing goes through the dashboard
        // `canvas.get` RPC only.
        "canvas_push" | "canvas_clear" => Some(Scope::WikiWrite),
        // ── Messaging / media egress ─────────────────────────────────────
        "send_message" | "send_photo" | "send_sticker" | "synthesize_speech"
        | "transcribe_audio" => Some(Scope::MessagingSend),
        // ── Agent Mail (P2-d) ────────────────────────────────────────────
        // Read and draft-send are separate grants. `mail_send` cannot
        // transmit (a human decision in `mail_worker::settle_outbox` does),
        // but queuing correspondence in front of a person is still an egress
        // -shaped act, so it gets its own scope rather than riding on
        // `MailRead`.
        "mail_list" | "mail_read" => Some(Scope::MailRead),
        "mail_send" => Some(Scope::MailSend),
        // RFC-21 §1: identity resolution requires its own scope so operators
        // can grant wiki access without exposing the person registry.
        "identity_resolve" => Some(Scope::IdentityRead),
        // RFC-21 §2: Odoo tool surface — three-tier scope split so an agent
        // granted only `odoo:read` cannot accidentally (or via prompt
        // injection) call mutating tools. These checks are defence-in-depth
        // *in addition to* the per-agent connector pool's `allowed_actions`
        // filter — both must pass.
        //
        // Read class: pure search_read / list / status.
        "odoo_status"
        | "odoo_crm_leads"
        | "odoo_sale_orders"
        | "odoo_inventory_products"
        | "odoo_inventory_check"
        | "odoo_invoice_list"
        | "odoo_payment_status"
        | "odoo_partner_search"
        | "odoo_schema_fields"
        | "odoo_search" => Some(Scope::OdooRead),
        // Connect is read-class — it acquires/refreshes the connection but
        // doesn't mutate Odoo state. Without it, no read can happen either.
        "odoo_connect" => Some(Scope::OdooRead),
        // Write class: create / write that mutate records but don't fire
        // workflow side-effects.
        "odoo_crm_create_lead"
        | "odoo_crm_update_stage"
        | "odoo_sale_create_quotation" => Some(Scope::OdooWrite),
        // Execute class: workflow buttons + generic execute_kw + report
        // generation. These can fire arbitrary Odoo-side actions.
        "odoo_sale_confirm" | "odoo_execute" | "odoo_report" => Some(Scope::OdooExecute),
        // WP-D §13.7: read-only SQL connector. One read scope for all four
        // tools — the connector has no write surface at all, so splitting a
        // `db:write` out would describe something that does not exist. The
        // dispatch gate ADDITIONALLY requires `[capabilities] db_sources` to
        // name the source (deny-by-default), so scope alone never reaches a
        // customer database.
        "db_sources" | "db_tables" | "db_select" | "db_query" => Some(Scope::DbRead),
        // WP-F2 §14.2: local data files. One read scope for all three — none
        // of them writes. There is no per-agent capability gate on top (unlike
        // the db family): `mcp_files::vet_path` confines every read to the
        // caller's own agent directory, `<home>/attachments`, and the
        // operator-declared `[files] allowed_roots`, which is the same
        // "whose data is this?" answer a capability list would give.
        "file_read" | "csv_read" | "xlsx_read" => Some(Scope::FilesRead),
        // Google Workspace native tools. Read class: connection diagnostics,
        // mail search/read, calendar listing, spreadsheet read — no external
        // side-effects.
        // Forms (structure + responses) and Google Tasks listing are read-only
        // too. `gtasks_*` is Google Tasks — distinct from DuDuClaw's own
        // `tasks_*` task-board tools.
        "google_status" | "gmail_search" | "gmail_read" | "calendar_list_events"
        | "sheets_read" | "forms_get" | "forms_list_responses" | "gtasks_lists"
        | "gtasks_list" | "drive_search" | "drive_read" | "docs_read" | "slides_read" => {
            Some(Scope::GoogleRead)
        }
        // Write class: draft creation (never sends) + real calendar-event
        // creation + spreadsheet row append + Google Tasks create/complete.
        // Defence-in-depth beyond any per-agent approval_required_tools gate
        // the operator adds.
        "gmail_create_draft" | "calendar_create_event" | "sheets_append" | "gtasks_create"
        | "gtasks_complete" | "docs_append" => Some(Scope::GoogleWrite),
        // Notion native tools. Read class: connection diagnostics, search, and
        // page read. Write class: append paragraph blocks to an existing page.
        "notion_status" | "notion_search" | "notion_page_read" => Some(Scope::NotionRead),
        "notion_page_append" => Some(Scope::NotionWrite),
        // GitHub native tools. Read class: diagnostics, issue/PR search and
        // read. Write class: post a publicly visible issue/PR comment.
        "github_status" | "github_search_issues" | "github_issue_read" | "github_pr_read" => {
            Some(Scope::GithubRead)
        }
        "github_issue_comment" => Some(Scope::GithubWrite),
        // W19-P1 M4: Audit Trail 查詢 API — admin-only，與 WebSocket 路徑
        // `require_admin!()` 保持對等訪問控制。
        "audit_trail_query" => Some(Scope::Admin),
        // W20-P0: Reliability Dashboard — admin-only，敏感指標資料。
        "reliability_summary" => Some(Scope::Admin),
        // R4 review: WebSocket dashboard requires manager+ for these via
        // `require_manager!()`; mirror as Admin scope at the MCP boundary
        // since MCP scopes lack a Manager tier. `wiki_trust_audit` exposes
        // page-level trust trends; `wiki_trust_history` exposes
        // `conversation_id` correlatable with user activity.
        "wiki_trust_audit" | "wiki_trust_history" => Some(Scope::Admin),
        // RFC-26: Live Run Forking surface. Gated by its own `fork:execute`
        // scope (defence-in-depth in addition to the per-agent `[fork] enabled`
        // toggle, which is checked at handler entry).
        "fork_run"
        | "inspect_branches"
        | "diff_branches"
        | "merge_or_select"
        | "terminate_branch"
        | "fork_cost" => Some(Scope::ForkExecute),
        // OS-native Phase 1: native notification, watch-status read, and open.
        // Gated by their own scope so OS integration can be granted without
        // Admin; the dispatch gate ALSO requires `[capabilities] os_native`.
        "os_notify" | "os_watch_status" | "os_open" => Some(Scope::OsNative),
        // OS-native P2-4: structured sensing sources (frontmost app/window,
        // Spotlight search, today's calendar events). Read-only — same scope
        // as the P1 tools, gated by [capabilities] os_native at the dispatch
        // gate; no ActionGuard (they have no host side-effect).
        "os_frontmost" | "os_spotlight_search" | "os_calendar_today" => Some(Scope::OsNative),
        // O-0: system-operator tool face bridging the dashboard-only
        // `device.*`/`system.*` RPCs to agents. Explicitly Admin-scoped
        // (matches the unmapped-tool fail-closed default byte-for-byte,
        // mapped here for clarity/lockability) — these operate the
        // physical/production machine, a strictly higher trust tier than
        // `os:native`'s host-automation tools. External clients can never
        // reach Admin (not in `EXTERNALLY_GRANTABLE_SCOPES`), so this
        // surface is internal-agent only. O-4 additionally requires the
        // agent's own explicit `agent.toml [capabilities] system_operator =
        // true` at the dispatch gate (`mcp_dispatch.rs`'s
        // `SYSTEM_OPERATOR_TOOLS` check) — Admin scope alone is no longer
        // sufficient, closing the "any internal agent could try these"
        // residual risk. Per-agent access is further scoped by `agent.toml
        // [capabilities] allowed_tools`/`denied_tools`.
        "os_device_status"
        | "os_system_status"
        | "os_check_update"
        | "os_backup_list"
        | "os_network_info"
        | "os_wifi_status"
        | "os_wifi_scan"
        | "os_wifi_connect"
        | "os_apply_update"
        | "os_boot_assessment"
        | "os_update_rollback"
        | "os_backup_create"
        | "os_power"
        | "os_factory_reset"
        | "os_doctor_repair"
        | "os_display_get"
        | "os_display_set"
        // Y10-1: agent→audio bridge (wpctl volume/mute/output), same tier
        // as os_display_get/set — see `duduclaw_gateway::audio_bridge`'s
        // module doc for why this never touches duduclaw-comp.
        | "os_audio_get"
        | "os_audio_set" => Some(Scope::Admin),
        // Server-side office-document script execution (docx/xlsx/pptx/pdf).
        // Its own least-privilege scope instead of the Admin `execute_program`
        // uses: the tool is constrained to the four bundled skills' vetted
        // scripts and the caller's agent directory.
        "office_script" => Some(Scope::SkillExecute),
        // WP3.3 recording-to-skill capture + distillation. Own scope so the
        // capability can be granted without Admin; the dispatch gate ALSO
        // requires `[capabilities] recording = true` (deny-by-default).
        "browser_record_start"
        | "browser_record_stop"
        | "desktop_record_start"
        | "desktop_record_stop"
        | "skill_from_recording" => Some(Scope::Recording),
        // CD-1 human-machine co-drive: GUI mouse/keyboard injection into a
        // shared desktop via `duduclaw-comp`. Explicitly Admin — the
        // highest internal trust tier, never externally grantable — same
        // tier as the other high-blast-radius tools below; enumerated on
        // its own line (not the Admin fall-through) so a future scope
        // split for co-drive is a one-line diff, not a silent behavior
        // change. The dispatch gate ADDITIONALLY requires the agent's own
        // `[capabilities] codrive = true` (deny-by-default, defence in
        // depth — see `mcp_dispatch.rs`'s `CODRIVE_TOOLS` check).
        //
        // A2: `codrive_status` is the read-only driving-state query on the
        // same socket. It is deliberately held to the SAME tier as
        // `codrive_run` rather than being softened for being a read: it
        // reveals whether a human is at the shared desktop right now, which
        // is exactly the signal an agent would want in order to time an
        // action around the human's absence. Enumerated on its own line for
        // the same reason `codrive_run` is — never the Admin fall-through.
        "codrive_run" | "codrive_status" => Some(Scope::Admin),
        // ── High-impact tools — explicitly Admin (C2 fix) ────────────────
        // Arbitrary code execution, agent lifecycle/identity mutation, prompt
        // rewrite, cross-agent dispatch, scheduling, and evolution control.
        // These previously fell through to `None` (no scope), letting any
        // narrowly-scoped internal key invoke them.
        // D1 source rollback: mass-expires facts + cascades trust downgrades —
        // high blast radius, so it requires Admin (the strictest reasonable
        // scope) rather than plain memory:write.
        "memory_invalidate_by_origin"
        | "execute_program"
        | "create_agent"
        | "spawn_agent"
        // O2 ephemeral synthesis: same blast radius as spawn_agent (agent
        // lifecycle + dispatch) — enumerated explicitly instead of relying
        // on the Admin fall-through (2026-07 scope-table consistency).
        | "spawn_ephemeral"
        | "agent_update"
        | "agent_update_soul"
        | "agent_remove"
        | "send_to_agent"
        | "evolution_toggle"
        | "delete_cron_task"
        | "update_cron_task"
        | "pause_cron_task"
        | "run_cron_task"
        | "channel_config"
        | "model_download"
        | "model_load"
        | "model_unload"
        | "llamafile_start"
        | "llamafile_stop"
        | "inference_mode"
        // Cost analytics comparison: enumerated explicitly at the same
        // effective scope it already had via the Admin default (the other
        // cost_* tools also resolve to Admin today).
        | "cost_multi_vs_single"
        | "skill_extract"
        | "skill_graduate"
        | "skill_security_scan"
        | "skill_synthesis_run"
        | "shared_skill_adopt"
        | "shared_skill_share" => Some(Scope::Admin),
        // Fail-closed: any tool not enumerated above requires Admin. See the
        // doc comment on this function.
        _ => Some(Scope::Admin),
    }
}

/// Argument-aware create scope; ordinary tasks retain their Admin contract.
pub fn tool_requires_scope_for_args(tool: &str, args: &serde_json::Value) -> Option<Scope> {
    if tool == "tasks_create" && args.get("kind").and_then(serde_json::Value::as_str)
        .is_some_and(|kind| kind.trim().eq_ignore_ascii_case("discovery")) {
        Some(Scope::DiscoveryExecute)
    } else { tool_requires_scope(tool) }
}
