//! Exact MCP resource policy and the trusted resource-resolution seam.
//!
//! This registry is independent of handler execution. Listing does not authorize
//! a call: each call resolves all of its resources and verifies the signed grants.
use duduclaw_isolation::capability::{EmployeeBinding, Principal, ResourceGrant};
use serde_json::Value;
use crate::{RequestContext, VerificationError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceKind { EmployeeScope, Employee, Workspace, Mailbox, Task, DatabaseTable, Wiki, Connector, Recipient, UrlOrigin, PublicOrigins, NewTask }
impl ResourceKind {
    pub fn of(resource: &ResourceGrant) -> Self {
        match resource {
            ResourceGrant::Confined { resource } => Self::of(resource),
            ResourceGrant::EmployeeScope { .. } => Self::EmployeeScope,
            ResourceGrant::Employee { .. } => Self::Employee,
            ResourceGrant::Workspace { .. } => Self::Workspace,
            ResourceGrant::Mailbox { .. } => Self::Mailbox,
            ResourceGrant::Task { .. } => Self::Task,
            ResourceGrant::DatabaseTable { .. } => Self::DatabaseTable,
            ResourceGrant::Wiki { .. } => Self::Wiki,
            ResourceGrant::Connector { .. } => Self::Connector,
            ResourceGrant::Recipient { .. } => Self::Recipient,
            ResourceGrant::UrlOrigin { .. } => Self::UrlOrigin,
            ResourceGrant::PublicOrigins { .. } => Self::PublicOrigins,
            ResourceGrant::NewTask { .. } => Self::NewTask,
        }
    }
}

/// The only tools a `PublicOrigins` grant (D-H2) can authorize. Their URL
/// argument must resolve to a granted public HTTPS origin; the host fetch
/// (`web_fetch_protected_cached`) pins DNS, refuses non-public addresses and
/// follows no redirect, so the grant never reaches a second origin.
pub const PUBLIC_ORIGIN_TOOLS: &[&str] = &["web_fetch_cached", "web_extract"];

/// The record's signed `PublicOrigins` grant, only for an employee-scope record.
fn public_origins_grant(context: &RequestContext) -> Option<&ResourceGrant> {
    let record = context.record();
    if !record.employee_scope() { return None; }
    record.resources.as_slice().iter().find(|grant| matches!(grant, ResourceGrant::PublicOrigins { .. }))
}

/// Execution and Worker MUST use packages A/E; neither may spawn under the
/// gateway uid. Connector execution MUST enforce the current binding/lease.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route { File, Service, Execution, Worker, Connector }
#[derive(Debug)]
pub struct ToolPolicy {
    pub name: &'static str,
    pub route: Route,
    pub required: &'static [ResourceKind],
}

pub const POLICIES: &[ToolPolicy] = &[
    // google
    ToolPolicy { name: "google_status", route: Route::Connector, required: &[ResourceKind::Employee, ResourceKind::Connector] },
    ToolPolicy { name: "gmail_search", route: Route::Connector, required: &[ResourceKind::Employee, ResourceKind::Connector, ResourceKind::Mailbox] },
    ToolPolicy { name: "gmail_read", route: Route::Connector, required: &[ResourceKind::Employee, ResourceKind::Connector, ResourceKind::Mailbox] },
    ToolPolicy { name: "gmail_create_draft", route: Route::Connector, required: &[ResourceKind::Employee, ResourceKind::Connector, ResourceKind::Mailbox, ResourceKind::Recipient] },
    // Google Chat reads return private conversation content: same exact
    // account-bound mailbox authority as gmail_read, never a shared fallback.
    ToolPolicy { name: "google_chat_spaces", route: Route::Connector, required: &[ResourceKind::Employee, ResourceKind::Connector, ResourceKind::Mailbox] },
    ToolPolicy { name: "google_chat_messages", route: Route::Connector, required: &[ResourceKind::Employee, ResourceKind::Connector, ResourceKind::Mailbox] },
    ToolPolicy { name: "calendar_list_events", route: Route::Connector, required: &[ResourceKind::Employee, ResourceKind::Connector] },
    ToolPolicy { name: "calendar_create_event", route: Route::Connector, required: &[ResourceKind::Employee, ResourceKind::Connector] },
    ToolPolicy { name: "sheets_read", route: Route::Connector, required: &[ResourceKind::Employee, ResourceKind::Connector] },
    ToolPolicy { name: "sheets_append", route: Route::Connector, required: &[ResourceKind::Employee, ResourceKind::Connector] },
    ToolPolicy { name: "forms_get", route: Route::Connector, required: &[ResourceKind::Employee, ResourceKind::Connector] },
    ToolPolicy { name: "forms_list_responses", route: Route::Connector, required: &[ResourceKind::Employee, ResourceKind::Connector] },
    ToolPolicy { name: "gtasks_lists", route: Route::Connector, required: &[ResourceKind::Employee, ResourceKind::Connector] },
    ToolPolicy { name: "gtasks_list", route: Route::Connector, required: &[ResourceKind::Employee, ResourceKind::Connector] },
    ToolPolicy { name: "gtasks_create", route: Route::Connector, required: &[ResourceKind::Employee, ResourceKind::Connector] },
    ToolPolicy { name: "gtasks_complete", route: Route::Connector, required: &[ResourceKind::Employee, ResourceKind::Connector] },
    ToolPolicy { name: "drive_search", route: Route::Connector, required: &[ResourceKind::Employee, ResourceKind::Connector] },
    ToolPolicy { name: "drive_read", route: Route::Connector, required: &[ResourceKind::Employee, ResourceKind::Connector] },
    ToolPolicy { name: "docs_read", route: Route::Connector, required: &[ResourceKind::Employee, ResourceKind::Connector] },
    ToolPolicy { name: "docs_append", route: Route::Connector, required: &[ResourceKind::Employee, ResourceKind::Connector] },
    ToolPolicy { name: "slides_read", route: Route::Connector, required: &[ResourceKind::Employee, ResourceKind::Connector] },
    // os
    ToolPolicy { name: "os_notify", route: Route::Execution, required: &[ResourceKind::Employee, ResourceKind::Recipient] },
    ToolPolicy { name: "os_watch_status", route: Route::Execution, required: &[ResourceKind::Employee] },
    ToolPolicy { name: "os_open", route: Route::Execution, required: &[ResourceKind::Employee, ResourceKind::Workspace] },
    ToolPolicy { name: "os_frontmost", route: Route::Execution, required: &[ResourceKind::Employee] },
    ToolPolicy { name: "os_spotlight_search", route: Route::Execution, required: &[ResourceKind::Employee, ResourceKind::Workspace] },
    ToolPolicy { name: "os_calendar_today", route: Route::Execution, required: &[ResourceKind::Employee] },
    ToolPolicy { name: "os_device_status", route: Route::Execution, required: &[ResourceKind::Employee] },
    ToolPolicy { name: "os_system_status", route: Route::Execution, required: &[ResourceKind::Employee] },
    ToolPolicy { name: "os_check_update", route: Route::Execution, required: &[ResourceKind::Employee] },
    ToolPolicy { name: "os_backup_list", route: Route::Execution, required: &[ResourceKind::Employee] },
    ToolPolicy { name: "os_network_info", route: Route::Execution, required: &[ResourceKind::Employee] },
    ToolPolicy { name: "os_wifi_status", route: Route::Execution, required: &[ResourceKind::Employee] },
    ToolPolicy { name: "os_wifi_scan", route: Route::Execution, required: &[ResourceKind::Employee] },
    ToolPolicy { name: "os_wifi_connect", route: Route::Execution, required: &[ResourceKind::Employee] },
    ToolPolicy { name: "os_apply_update", route: Route::Execution, required: &[ResourceKind::Employee] },
    ToolPolicy { name: "os_boot_assessment", route: Route::Execution, required: &[ResourceKind::Employee] },
    ToolPolicy { name: "os_update_rollback", route: Route::Execution, required: &[ResourceKind::Employee] },
    ToolPolicy { name: "os_backup_create", route: Route::Execution, required: &[ResourceKind::Employee] },
    ToolPolicy { name: "os_power", route: Route::Execution, required: &[ResourceKind::Employee] },
    ToolPolicy { name: "os_factory_reset", route: Route::Execution, required: &[ResourceKind::Employee] },
    ToolPolicy { name: "os_doctor_repair", route: Route::Execution, required: &[ResourceKind::Employee] },
    ToolPolicy { name: "os_display_get", route: Route::Execution, required: &[ResourceKind::Employee] },
    ToolPolicy { name: "os_display_set", route: Route::Execution, required: &[ResourceKind::Employee] },
    ToolPolicy { name: "os_audio_get", route: Route::Execution, required: &[ResourceKind::Employee] },
    ToolPolicy { name: "os_audio_set", route: Route::Execution, required: &[ResourceKind::Employee] },
    // agents
    ToolPolicy { name: "create_agent", route: Route::Service, required: &[ResourceKind::Employee] },
    ToolPolicy { name: "list_agents", route: Route::Service, required: &[ResourceKind::Employee] },
    // H8: a TaskSpec plan is host-queue work under the run's own live task
    // (`create_task_selector`), not a workspace operation. An interactive run
    // instead uses its signed `NewTask` grant (`new_task_grant`, H9 decision 3);
    // `required` lists the record-task path.
    ToolPolicy { name: "create_task", route: Route::Execution, required: &[ResourceKind::Employee, ResourceKind::Task] },
    ToolPolicy { name: "check_responses", route: Route::Service, required: &[ResourceKind::Employee] },
    ToolPolicy { name: "task_status", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::Task] },
    ToolPolicy { name: "agent_status", route: Route::Service, required: &[ResourceKind::Employee] },
    ToolPolicy { name: "spawn_agent", route: Route::Execution, required: &[ResourceKind::Employee, ResourceKind::Workspace] },
    ToolPolicy { name: "spawn_ephemeral", route: Route::Execution, required: &[ResourceKind::Employee, ResourceKind::Workspace] },
    ToolPolicy { name: "agent_update", route: Route::Service, required: &[ResourceKind::Employee] },
    ToolPolicy { name: "agent_remove", route: Route::Service, required: &[ResourceKind::Employee] },
    ToolPolicy { name: "agent_update_soul", route: Route::Service, required: &[ResourceKind::Employee] },
    // odoo
    ToolPolicy { name: "odoo_connect", route: Route::Connector, required: &[ResourceKind::Employee, ResourceKind::Connector] },
    ToolPolicy { name: "odoo_status", route: Route::Connector, required: &[ResourceKind::Employee, ResourceKind::Connector] },
    ToolPolicy { name: "odoo_crm_leads", route: Route::Connector, required: &[ResourceKind::Employee, ResourceKind::Connector] },
    ToolPolicy { name: "odoo_crm_create_lead", route: Route::Connector, required: &[ResourceKind::Employee, ResourceKind::Connector] },
    ToolPolicy { name: "odoo_crm_update_stage", route: Route::Connector, required: &[ResourceKind::Employee, ResourceKind::Connector] },
    ToolPolicy { name: "odoo_sale_orders", route: Route::Connector, required: &[ResourceKind::Employee, ResourceKind::Connector] },
    ToolPolicy { name: "odoo_sale_create_quotation", route: Route::Connector, required: &[ResourceKind::Employee, ResourceKind::Connector] },
    ToolPolicy { name: "odoo_sale_confirm", route: Route::Connector, required: &[ResourceKind::Employee, ResourceKind::Connector] },
    ToolPolicy { name: "odoo_inventory_products", route: Route::Connector, required: &[ResourceKind::Employee, ResourceKind::Connector] },
    ToolPolicy { name: "odoo_inventory_check", route: Route::Connector, required: &[ResourceKind::Employee, ResourceKind::Connector] },
    ToolPolicy { name: "odoo_invoice_list", route: Route::Connector, required: &[ResourceKind::Employee, ResourceKind::Connector] },
    ToolPolicy { name: "odoo_payment_status", route: Route::Connector, required: &[ResourceKind::Employee, ResourceKind::Connector] },
    ToolPolicy { name: "odoo_search", route: Route::Connector, required: &[ResourceKind::Employee, ResourceKind::Connector] },
    ToolPolicy { name: "odoo_execute", route: Route::Connector, required: &[ResourceKind::Employee, ResourceKind::Connector] },
    ToolPolicy { name: "odoo_report", route: Route::Connector, required: &[ResourceKind::Employee, ResourceKind::Connector] },
    ToolPolicy { name: "odoo_partner_search", route: Route::Connector, required: &[ResourceKind::Employee, ResourceKind::Connector] },
    ToolPolicy { name: "odoo_schema_overview", route: Route::Connector, required: &[ResourceKind::Employee, ResourceKind::Connector] },
    ToolPolicy { name: "odoo_schema_fields", route: Route::Connector, required: &[ResourceKind::Employee, ResourceKind::Connector] },
    // RM-120 generic create / write: the employee's own Odoo connector; the
    // explicit-grant gate chain runs in the bound handler (owner decision C).
    ToolPolicy { name: "odoo_create", route: Route::Connector, required: &[ResourceKind::Employee, ResourceKind::Connector] },
    ToolPolicy { name: "odoo_write", route: Route::Connector, required: &[ResourceKind::Employee, ResourceKind::Connector] },
    // exec
    // H12a: own skill tools read host-owned skill files of the record's own
    // employee and publish only into its own wiki/SKILLS; no workspace input.
    ToolPolicy { name: "skill_extract", route: Route::Worker, required: &[ResourceKind::Employee, ResourceKind::Wiki] },
    ToolPolicy { name: "execute_program", route: Route::Execution, required: &[ResourceKind::Employee, ResourceKind::Workspace] },
    ToolPolicy { name: "office_script", route: Route::Execution, required: &[ResourceKind::Employee, ResourceKind::Workspace] },
    ToolPolicy { name: "skill_bank_feedback", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::Wiki] },
    ToolPolicy { name: "computer_screenshot", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::Workspace] },
    ToolPolicy { name: "computer_click", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::Workspace] },
    ToolPolicy { name: "computer_type", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::Workspace] },
    ToolPolicy { name: "computer_login", route: Route::Connector, required: &[ResourceKind::Employee, ResourceKind::Workspace, ResourceKind::Connector] },
    ToolPolicy { name: "computer_key", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::Workspace] },
    ToolPolicy { name: "computer_scroll", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::Workspace] },
    ToolPolicy { name: "computer_session_start", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::Workspace] },
    ToolPolicy { name: "computer_navigate", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::Workspace] },
    ToolPolicy { name: "computer_session_stop", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::Workspace] },
    ToolPolicy { name: "computer_workspace_list", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::Workspace] },
    ToolPolicy { name: "computer_workspace_read", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::Workspace] },
    ToolPolicy { name: "computer_workspace_write", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::Workspace] },
    ToolPolicy { name: "session_restore_context", route: Route::Service, required: &[ResourceKind::Employee] },
    // fork
    ToolPolicy { name: "fork_run", route: Route::Execution, required: &[ResourceKind::Employee, ResourceKind::Workspace] },
    ToolPolicy { name: "inspect_branches", route: Route::Execution, required: &[ResourceKind::Employee, ResourceKind::Workspace] },
    ToolPolicy { name: "diff_branches", route: Route::Execution, required: &[ResourceKind::Employee, ResourceKind::Workspace] },
    ToolPolicy { name: "merge_or_select", route: Route::Execution, required: &[ResourceKind::Employee, ResourceKind::Workspace] },
    ToolPolicy { name: "terminate_branch", route: Route::Execution, required: &[ResourceKind::Employee, ResourceKind::Workspace] },
    ToolPolicy { name: "fork_cost", route: Route::Execution, required: &[ResourceKind::Employee, ResourceKind::Workspace] },
    // tasks
    ToolPolicy { name: "credential_declare", route: Route::Service, required: &[ResourceKind::Employee] },
    ToolPolicy { name: "discovery_catalog", route: Route::Execution, required: &[ResourceKind::Employee, ResourceKind::Workspace] },
    ToolPolicy { name: "discovery_list", route: Route::Execution, required: &[ResourceKind::Employee, ResourceKind::Workspace] },
    ToolPolicy { name: "discovery_tree", route: Route::Execution, required: &[ResourceKind::Employee, ResourceKind::Workspace] },
    ToolPolicy { name: "discovery_artifact", route: Route::Execution, required: &[ResourceKind::Employee, ResourceKind::Workspace] },
    ToolPolicy { name: "discovery_cancel", route: Route::Execution, required: &[ResourceKind::Employee, ResourceKind::Workspace] },
    ToolPolicy { name: "tasks_list", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::Task] },
    ToolPolicy { name: "tasks_create", route: Route::Service, required: &[ResourceKind::Employee] },
    ToolPolicy { name: "tasks_update", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::Task] },
    ToolPolicy { name: "tasks_claim", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::Task] },
    ToolPolicy { name: "tasks_renew", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::Task] },
    ToolPolicy { name: "tasks_complete", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::Task] },
    ToolPolicy { name: "responsibility_get", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::Task] },
    ToolPolicy { name: "responsibility_followup", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::Task] },
    ToolPolicy { name: "responsibility_ask", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::Task] },
    ToolPolicy { name: "tasks_block", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::Task] },
    ToolPolicy { name: "capability_request", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::Task] },
    ToolPolicy { name: "goals_create", route: Route::Service, required: &[ResourceKind::Employee] },
    ToolPolicy { name: "goals_list", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::Task] },
    ToolPolicy { name: "plan_get", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::Task] },
    ToolPolicy { name: "plan_update_step", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::Task] },
    ToolPolicy { name: "activity_post", route: Route::Service, required: &[ResourceKind::Employee] },
    ToolPolicy { name: "activity_list", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::Task] },
    ToolPolicy { name: "autopilot_list", route: Route::Service, required: &[ResourceKind::Employee] },
    ToolPolicy { name: "shared_skill_list", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::Wiki] },
    ToolPolicy { name: "shared_skill_share", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::Wiki] },
    ToolPolicy { name: "shared_skill_adopt", route: Route::Worker, required: &[ResourceKind::Employee] },
    ToolPolicy { name: "plan_start", route: Route::Service, required: &[ResourceKind::Employee] },
    ToolPolicy { name: "memory_improve", route: Route::Service, required: &[ResourceKind::Employee] },
    // recording
    ToolPolicy { name: "browser_record_start", route: Route::Execution, required: &[ResourceKind::Employee, ResourceKind::Workspace] },
    ToolPolicy { name: "browser_record_stop", route: Route::Execution, required: &[ResourceKind::Employee, ResourceKind::Workspace] },
    ToolPolicy { name: "desktop_record_start", route: Route::Execution, required: &[ResourceKind::Employee, ResourceKind::Workspace] },
    ToolPolicy { name: "desktop_record_stop", route: Route::Execution, required: &[ResourceKind::Employee, ResourceKind::Workspace] },
    ToolPolicy { name: "skill_from_recording", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::Workspace] },
    // memory
    ToolPolicy { name: "memory_search", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::DatabaseTable] },
    ToolPolicy { name: "memory_store", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::DatabaseTable] },
    ToolPolicy { name: "working_state_set", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::DatabaseTable] },
    ToolPolicy { name: "working_state_clear", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::DatabaseTable] },
    ToolPolicy { name: "working_state_handoff", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::DatabaseTable] },
    ToolPolicy { name: "working_state_get", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::DatabaseTable] },
    ToolPolicy { name: "team_handoff", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::Mailbox, ResourceKind::Recipient] },
    ToolPolicy { name: "belief_submit", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::DatabaseTable] },
    ToolPolicy { name: "belief_settle", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::DatabaseTable] },
    ToolPolicy { name: "belief_stats", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::DatabaseTable] },
    ToolPolicy { name: "mail_list", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::Mailbox] },
    ToolPolicy { name: "mail_read", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::Mailbox] },
    ToolPolicy { name: "mail_send", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::Mailbox, ResourceKind::Recipient] },
    ToolPolicy { name: "codrive_run", route: Route::Execution, required: &[ResourceKind::Employee, ResourceKind::Workspace] },
    ToolPolicy { name: "codrive_status", route: Route::Execution, required: &[ResourceKind::Employee, ResourceKind::Workspace] },
    ToolPolicy { name: "memory_read", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::DatabaseTable] },
    ToolPolicy { name: "memory_fetch_batch", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::DatabaseTable] },
    ToolPolicy { name: "memory_alias_add", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::DatabaseTable] },
    ToolPolicy { name: "memory_alias_list", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::DatabaseTable] },
    ToolPolicy { name: "memory_get_history", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::DatabaseTable] },
    ToolPolicy { name: "memory_get_at", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::DatabaseTable] },
    ToolPolicy { name: "memory_invalidate_by_origin", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::DatabaseTable] },
    ToolPolicy { name: "memory_search_by_layer", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::DatabaseTable] },
    ToolPolicy { name: "code_map", route: Route::Execution, required: &[ResourceKind::Employee, ResourceKind::Workspace] },
    ToolPolicy { name: "user_profile_record", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::DatabaseTable] },
    ToolPolicy { name: "user_profile_get", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::DatabaseTable] },
    ToolPolicy { name: "user_code_profile", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::DatabaseTable] },
    ToolPolicy { name: "memory_successful_conversations", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::DatabaseTable] },
    ToolPolicy { name: "memory_episodic_pressure", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::DatabaseTable] },
    ToolPolicy { name: "memory_consolidation_status", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::DatabaseTable] },
    ToolPolicy { name: "decision_list", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::DatabaseTable] },
    ToolPolicy { name: "decision_resolve", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::DatabaseTable] },
    // skills
    ToolPolicy { name: "skill_search", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::Wiki] },
    ToolPolicy { name: "skill_list", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::Wiki] },
    ToolPolicy { name: "skill_gaps", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::Wiki] },
    ToolPolicy { name: "skill_security_scan", route: Route::Worker, required: &[ResourceKind::Employee] },
    ToolPolicy { name: "skill_graduate", route: Route::Worker, required: &[ResourceKind::Employee] },
    ToolPolicy { name: "skill_synthesis_status", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::Wiki] },
    ToolPolicy { name: "skill_synthesis_run", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::Workspace] },
    ToolPolicy { name: "skill_hub_install", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::Workspace] },
    ToolPolicy { name: "skill_curator_status", route: Route::Service, required: &[ResourceKind::Employee] },
    ToolPolicy { name: "skill_pin", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::Wiki] },
    // integrations
    ToolPolicy { name: "notion_status", route: Route::Connector, required: &[ResourceKind::Employee, ResourceKind::Connector] },
    ToolPolicy { name: "notion_search", route: Route::Connector, required: &[ResourceKind::Employee, ResourceKind::Connector] },
    ToolPolicy { name: "notion_page_read", route: Route::Connector, required: &[ResourceKind::Employee, ResourceKind::Connector] },
    ToolPolicy { name: "notion_page_append", route: Route::Connector, required: &[ResourceKind::Employee, ResourceKind::Connector] },
    ToolPolicy { name: "github_status", route: Route::Connector, required: &[ResourceKind::Employee, ResourceKind::Connector] },
    ToolPolicy { name: "github_search_issues", route: Route::Connector, required: &[ResourceKind::Employee, ResourceKind::Connector] },
    ToolPolicy { name: "github_issue_read", route: Route::Connector, required: &[ResourceKind::Employee, ResourceKind::Connector] },
    ToolPolicy { name: "github_pr_read", route: Route::Connector, required: &[ResourceKind::Employee, ResourceKind::Connector] },
    ToolPolicy { name: "github_issue_comment", route: Route::Connector, required: &[ResourceKind::Employee, ResourceKind::Connector] },
    ToolPolicy { name: "cost_summary", route: Route::Service, required: &[ResourceKind::Employee] },
    ToolPolicy { name: "cost_agents", route: Route::Service, required: &[ResourceKind::Employee] },
    ToolPolicy { name: "cost_users", route: Route::Service, required: &[ResourceKind::Employee] },
    ToolPolicy { name: "cost_recent", route: Route::Service, required: &[ResourceKind::Employee] },
    ToolPolicy { name: "cost_multi_vs_single", route: Route::Service, required: &[ResourceKind::Employee] },
    ToolPolicy { name: "transcribe_audio", route: Route::Worker, required: &[ResourceKind::Employee] },
    ToolPolicy { name: "meeting_transcribe", route: Route::Service, required: &[ResourceKind::Employee] },
    // Job control uses the authenticated gateway meeting-jobs route. These
    // operations have no media input; only transcription parsing belongs in E.
    ToolPolicy { name: "meeting_upload_link", route: Route::Service, required: &[ResourceKind::Employee] },
    ToolPolicy { name: "meeting_transcribe_status", route: Route::Service, required: &[ResourceKind::Employee] },
    ToolPolicy { name: "meeting_transcribe_cancel", route: Route::Service, required: &[ResourceKind::Employee] },
    ToolPolicy { name: "synthesize_speech", route: Route::Worker, required: &[ResourceKind::Employee] },
    // knowledge
    ToolPolicy { name: "knowledge_search", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::Wiki] },
    ToolPolicy { name: "knowledge_get_passage", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::Wiki] },
    ToolPolicy { name: "knowledge_list_collections", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::Wiki] },
    ToolPolicy { name: "knowledge_add_source", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::Wiki] },
    // wiki
    ToolPolicy { name: "wiki_ls", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::Wiki] },
    ToolPolicy { name: "wiki_read", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::Wiki] },
    ToolPolicy { name: "wiki_write", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::Wiki] },
    ToolPolicy { name: "wiki_search", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::Wiki] },
    ToolPolicy { name: "wiki_lint", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::Wiki] },
    ToolPolicy { name: "wiki_stats", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::Wiki] },
    ToolPolicy { name: "wiki_export", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::Wiki] },
    ToolPolicy { name: "wiki_dedup", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::Wiki] },
    ToolPolicy { name: "wiki_graph", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::Wiki] },
    ToolPolicy { name: "wiki_rebuild_fts", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::Wiki] },
    ToolPolicy { name: "wiki_trust_audit", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::Wiki] },
    ToolPolicy { name: "wiki_trust_history", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::Wiki] },
    ToolPolicy { name: "shared_wiki_delete", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::Wiki] },
    ToolPolicy { name: "wiki_namespace_status", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::Wiki] },
    ToolPolicy { name: "canvas_push", route: Route::Service, required: &[ResourceKind::Employee] },
    ToolPolicy { name: "canvas_clear", route: Route::Service, required: &[ResourceKind::Employee] },
    ToolPolicy { name: "identity_resolve", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::Recipient] },
    ToolPolicy { name: "wiki_share", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::Wiki] },
    // data
    ToolPolicy { name: "db_sources", route: Route::Connector, required: &[ResourceKind::Employee, ResourceKind::DatabaseTable, ResourceKind::Connector] },
    ToolPolicy { name: "db_tables", route: Route::Connector, required: &[ResourceKind::Employee, ResourceKind::DatabaseTable, ResourceKind::Connector] },
    ToolPolicy { name: "db_select", route: Route::Connector, required: &[ResourceKind::Employee, ResourceKind::DatabaseTable, ResourceKind::Connector] },
    ToolPolicy { name: "db_query", route: Route::Connector, required: &[ResourceKind::Employee, ResourceKind::DatabaseTable, ResourceKind::Connector] },
    ToolPolicy { name: "file_read", route: Route::File, required: &[ResourceKind::Workspace] },
    ToolPolicy { name: "csv_read", route: Route::File, required: &[ResourceKind::Workspace] },
    ToolPolicy { name: "xlsx_read", route: Route::File, required: &[ResourceKind::Workspace] },
    // channels
    ToolPolicy { name: "group_history_read", route: Route::Connector, required: &[ResourceKind::Employee, ResourceKind::Connector, ResourceKind::Mailbox] },
    ToolPolicy { name: "send_message", route: Route::Connector, required: &[ResourceKind::Employee, ResourceKind::Connector, ResourceKind::Recipient] },
    ToolPolicy { name: "send_photo", route: Route::Connector, required: &[ResourceKind::Employee, ResourceKind::Connector, ResourceKind::Recipient] },
    ToolPolicy { name: "send_sticker", route: Route::Connector, required: &[ResourceKind::Employee, ResourceKind::Connector, ResourceKind::Recipient] },
    ToolPolicy { name: "web_search", route: Route::Connector, required: &[ResourceKind::Employee, ResourceKind::Connector] },
    ToolPolicy { name: "send_to_agent", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::Mailbox, ResourceKind::Recipient] },
    ToolPolicy { name: "list_cron_tasks", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::Task] },
    ToolPolicy { name: "update_cron_task", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::Task] },
    ToolPolicy { name: "delete_cron_task", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::Task] },
    ToolPolicy { name: "pause_cron_task", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::Task] },
    ToolPolicy { name: "run_cron_task", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::Task] },
    ToolPolicy { name: "create_reminder", route: Route::Service, required: &[ResourceKind::Employee] },
    ToolPolicy { name: "list_reminders", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::Task] },
    ToolPolicy { name: "cancel_reminder", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::Task] },
    // inference
    // Fetching raw bytes stays on the host under signed-origin and egress
    // policy. Interpreting untrusted HTML remains a separate E operation.
    ToolPolicy { name: "web_fetch_cached", route: Route::Service, required: &[ResourceKind::Employee, ResourceKind::UrlOrigin] },
    ToolPolicy { name: "web_extract", route: Route::Worker, required: &[ResourceKind::Employee, ResourceKind::UrlOrigin] },
    ToolPolicy { name: "inference_status", route: Route::Service, required: &[ResourceKind::Employee] },
    ToolPolicy { name: "model_list", route: Route::Service, required: &[ResourceKind::Employee] },
    ToolPolicy { name: "model_load", route: Route::Worker, required: &[ResourceKind::Employee, ResourceKind::Workspace] },
    ToolPolicy { name: "model_unload", route: Route::Worker, required: &[ResourceKind::Employee, ResourceKind::Workspace] },
    ToolPolicy { name: "hardware_info", route: Route::Service, required: &[ResourceKind::Employee] },
    // Host configuration selects a route without loading a model or generating.
    ToolPolicy { name: "route_query", route: Route::Service, required: &[ResourceKind::Employee] },
    ToolPolicy { name: "inference_mode", route: Route::Service, required: &[ResourceKind::Employee] },
    ToolPolicy { name: "llamafile_start", route: Route::Worker, required: &[ResourceKind::Employee, ResourceKind::Workspace] },
    ToolPolicy { name: "llamafile_stop", route: Route::Worker, required: &[ResourceKind::Employee, ResourceKind::Workspace] },
    ToolPolicy { name: "llamafile_list", route: Route::Service, required: &[ResourceKind::Employee] },
    ToolPolicy { name: "model_search", route: Route::Connector, required: &[ResourceKind::Employee, ResourceKind::Connector] },
    ToolPolicy { name: "model_download", route: Route::Worker, required: &[ResourceKind::Employee, ResourceKind::Workspace] },
    ToolPolicy { name: "model_recommend", route: Route::Connector, required: &[ResourceKind::Employee, ResourceKind::Connector] },
    // ops
    ToolPolicy { name: "submit_feedback", route: Route::Service, required: &[ResourceKind::Employee] },
    ToolPolicy { name: "evolution_toggle", route: Route::Service, required: &[ResourceKind::Employee] },
    ToolPolicy { name: "evolution_status", route: Route::Service, required: &[ResourceKind::Employee] },
    ToolPolicy { name: "audit_trail_query", route: Route::Service, required: &[ResourceKind::Employee] },
    ToolPolicy { name: "reliability_summary", route: Route::Service, required: &[ResourceKind::Employee] },
    ToolPolicy { name: "channel_config", route: Route::Connector, required: &[ResourceKind::Employee, ResourceKind::Connector] },
    ToolPolicy { name: "channel_config_list", route: Route::Connector, required: &[ResourceKind::Employee, ResourceKind::Connector] },
    ToolPolicy { name: "channel_status", route: Route::Connector, required: &[ResourceKind::Employee, ResourceKind::Connector] },
    ToolPolicy { name: "pairing_manage", route: Route::Connector, required: &[ResourceKind::Employee, ResourceKind::Connector] },
];

pub fn policy(name: &str) -> Option<&'static ToolPolicy> {
    POLICIES.iter().find(|policy| policy.name == name)
}

/// Host implementation contract, never supplied by MCP or employee code:
/// resolve every selected resource using trusted registries and live policy.
/// Include nested recipients, task dependencies, target employees, connector
/// credentials and all database tables (including views, joins and triggers).
/// A query/list resolves its finite authorized collection before execution;
/// results MUST be restricted to that collection. Unknown/ambiguous selectors,
/// unavailable stores and over-limit collections return an error. No wildcard
/// grants, caller-provided OS paths, cached authorization or schema-only guesses.
/// Resource names are not authority: for example `source` resolves through the
/// employee's database registry, never an arbitrary connection string.
///
/// D verifies the resulting exact grants; this seam is part of the trusted host
/// resource service. It MUST NOT delegate resolution to the untrusted worker.
pub trait ResourceResolver: Send + Sync {
    fn resolve(&self, context: &RequestContext, policy: &ToolPolicy, arguments: &Value)
        -> Result<Vec<ResourceGrant>, VerificationError>;
}

/// Only this module constructs a call. Consumers execute against `resources()`
/// and `context()`, not a second interpretation of authority in the arguments.
/// They must recheck current leases and enforce row/recipient/namespace bounds at
/// use, including redirects and secondary I/O; failure cannot reach a legacy
/// unscoped handler. Paths are opened through ResourceRoots, never reopened.
#[derive(Debug, Clone)]
pub struct AuthorizedToolCall {
    context: RequestContext,
    policy: &'static ToolPolicy,
    arguments: Value,
    resources: Vec<ResourceGrant>,
}
impl AuthorizedToolCall {
    pub fn employee_scope(&self) -> bool { self.context.record().employee_scope() }
    pub fn context(&self) -> &RequestContext { &self.context }
    pub fn policy(&self) -> &'static ToolPolicy { self.policy }
    pub fn name(&self) -> &'static str { self.policy.name }
    pub fn arguments(&self) -> &Value { &self.arguments }
    pub fn resources(&self) -> &[ResourceGrant] { &self.resources }
}

/// These service routes orchestrate E stages, but retain the finite resource
/// authority required when they were direct worker routes.
fn employee_service_scope(policy: &ToolPolicy) -> bool {
    policy.route == Route::Service && !matches!(policy.name,
        "skill_from_recording" | "skill_synthesis_run" | "skill_hub_install" |
        "skill_curator_status" | "meeting_transcribe" | "meeting_transcribe_status" |
        "meeting_transcribe_cancel" | "meeting_upload_link" | "knowledge_add_source" |
        "web_fetch_cached" | "route_query")
}

fn own_employee(context: &RequestContext) -> ResourceGrant {
    ResourceGrant::Employee { id: context.record().employee }
}

/// Top-level task selector argument keys. `create_task` never accepts them.
pub const TASK_SELECTOR_KEYS: [&str; 5] = ["task_id", "parent_task_id", "goal_id", "parent_goal_id", "depends_on"];

/// H8 live task selector for `create_task`: the task is the run's own task
/// from the verified capability record (`record.task`), never an argument, and
/// the record must also carry the exact signed `Task` grant for it. Live
/// ownership (the task store says the calling employee owns it now) is the
/// resolver's job at use time; this is only the record-side half, shared by
/// discovery (`eligible`) and `authorize`.
pub fn create_task_selector(context: &RequestContext) -> Result<ResourceGrant, VerificationError> {
    let record = context.record();
    let task = record.task.as_ref().ok_or(VerificationError::Invalid)?;
    let selector = ResourceGrant::Task { id: task.clone() };
    validate_resource(&selector)?;
    if !record.io_resources().any(|grant| grant == &selector) { return Err(VerificationError::Invalid); }
    Ok(selector)
}

/// H12a own-skill tools. They act only on the record's own employee: its
/// host-owned skill files (`agents/<label>/SKILLS`, read by the trusted
/// producer and parsed as data by the E worker) and its own wiki/SKILLS
/// publication. Identity is the verified record (EmployeeScope + its own
/// `Employee` grant); the agent label is bound by the host producer, never an
/// argument. Task-limited records (no EmployeeScope) never list them.
pub const OWN_SKILL_TOOLS: &[&str] = &["skill_extract", "shared_skill_adopt", "skill_security_scan",
    "skill_graduate", "skill_curator_status"];

/// The exact argument schema of [`OWN_SKILL_TOOLS`] in protected mode. No
/// selector names another employee: `target_agent`, `scope`, `namespace`,
/// paths and execution identities refuse. `agent_id` is accepted only as the
/// legacy schema's self-reference; the producer requires it to equal the bound
/// employee. Values are short slugs, so skill content never travels here.
pub fn check_own_skill_arguments(name: &str, arguments: &Value) -> Result<(), VerificationError> {
    let object = arguments.as_object().ok_or(VerificationError::Invalid)?;
    let allowed: &[&str] = match name {
        "skill_curator_status" => &["run"],
        "shared_skill_adopt" => &["skill_name"],
        "skill_extract" | "skill_security_scan" | "skill_graduate" => &["skill_name", "agent_id"],
        _ => return Err(VerificationError::Invalid),
    };
    if object.keys().any(|key| !allowed.contains(&key.as_str())) { return Err(VerificationError::Invalid); }
    if object.get("run").is_some_and(|value| !value.is_boolean()) { return Err(VerificationError::Invalid); }
    let slug = |value: &Value| value.as_str().is_some_and(|s| !s.is_empty() && s.len() <= 64
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'));
    if name != "skill_curator_status" && !object.get("skill_name").is_some_and(slug) {
        return Err(VerificationError::Invalid);
    }
    if object.get("agent_id").is_some_and(|value| !slug(value)) { return Err(VerificationError::Invalid); }
    Ok(())
}

/// Record-side selection for [`OWN_SKILL_TOOLS`], shared by discovery and
/// calls: the employee's own `Employee` grant under its own EmployeeScope,
/// plus (skill_extract only) exactly one own-namespace wiki output grant.
/// Which `agent:<label>` namespace is "own" is the host resolver's and the
/// producer's decision (`verify_wiki_root`); here only the shape is pinned.
pub fn own_skill_selector(context: &RequestContext, name: &str) -> Result<Vec<ResourceGrant>, VerificationError> {
    if !OWN_SKILL_TOOLS.contains(&name) { return Err(VerificationError::Invalid); }
    let record = context.record();
    let own = own_employee(context);
    if !record.employee_scope() || !record.io_resources().any(|grant| grant == &own) {
        return Err(VerificationError::Invalid);
    }
    let mut selection = vec![own];
    if name == "skill_extract" {
        let wikis: Vec<_> = record.io_resources().filter(|grant| matches!(grant, ResourceGrant::Wiki { namespace }
            if namespace.as_str().strip_prefix("agent:").is_some_and(|label| !label.is_empty()))
            && validate_resource(grant).is_ok()).collect();
        let [wiki] = wikis.as_slice() else { return Err(VerificationError::Invalid); };
        selection.push((*wiki).clone());
    }
    Ok(selection)
}

/// H9 owner decision 3: the interactive-run `create_task` authority. The record
/// must be an employee-scope record without a task, its principal a `Human`,
/// and it must carry the plain (never `Confined`) signed
/// `NewTask { requester }` grant for exactly that human. The requester is the
/// authenticated person of the run; it is never read from arguments. A
/// Background principal never qualifies (the capability decoder refuses such
/// a grant too).
pub fn new_task_grant(context: &RequestContext) -> Result<ResourceGrant, VerificationError> {
    let record = context.record();
    let Principal::Human { user_id, .. } = &record.principal else { return Err(VerificationError::Invalid); };
    if record.task.is_some() || !record.employee_scope() { return Err(VerificationError::Invalid); }
    let grant = ResourceGrant::NewTask { requester: user_id.clone() };
    validate_resource(&grant)?;
    if !record.resources.as_slice().contains(&grant) { return Err(VerificationError::Invalid); }
    Ok(grant)
}

/// Top-level `create_task` arguments a `NewTask` call may carry. Everything
/// else (task/goal/dependency selectors, assignee or creator fields, identity
/// keys) is refused instead of being ignored.
pub const NEW_TASK_KEYS: [&str; 2] = ["goal", "steps"];
/// Keys a `NewTask` plan step may carry (the existing step schema). The step
/// `agent` must name the calling employee or be empty; the resolver, which
/// knows the employee's agent label, enforces that.
pub const NEW_TASK_STEP_KEYS: [&str; 4] = ["description", "agent", "depends_on", "acceptance_criteria"];
/// Most steps in one interactive plan.
pub const NEW_TASK_MAX_STEPS: usize = 64;

/// The plan steps of a `create_task` call, parsed exactly as the handler parses
/// them (a JSON array, or a JSON string holding one), each a JSON object.
pub fn new_task_steps(arguments: &Value) -> Result<Vec<Value>, VerificationError> {
    let steps = match arguments.get("steps").ok_or(VerificationError::Invalid)? {
        Value::Array(steps) => steps.clone(),
        Value::String(raw) => serde_json::from_str::<Vec<Value>>(raw).map_err(|_| VerificationError::Invalid)?,
        _ => return Err(VerificationError::Invalid),
    };
    if steps.is_empty() || steps.len() > NEW_TASK_MAX_STEPS || steps.iter().any(|step| !step.is_object()) {
        return Err(VerificationError::Invalid);
    }
    Ok(steps)
}

/// Discovery shares the exact tool and mandatory resource policy with calls.
/// The transport additionally checks that the corresponding host service exists.
/// Optional argument-dependent resources are checked by `authorize`, not guessed
/// while listing (the same principle as existing argument-specific tool gates).
pub fn eligible(context: &RequestContext, name: &str) -> bool {
    let Some(policy) = policy(name) else { return false; };
    let record = context.record();
    if !(record.validate_resource_mode().is_ok() && record.tools.as_slice().iter().any(|tool| tool.as_str() == name)) {
        return false;
    }
    if name == "create_task" {
        // Either the run's own signed record task (H8; task/schedule runs) or
        // the interactive run's signed NewTask grant, always with the own
        // Employee grant. Live ownership/requester checks run per call.
        return record.io_resources().any(|grant| grant == &own_employee(context))
            && (create_task_selector(context).is_ok() || new_task_grant(context).is_ok());
    }
    // D-H2: the URL itself is matched against the grant by `authorize`.
    // H12a: own-skill tools additionally need their record-side selector
    // (applies to the whole disjunction, not only the required-kind branch).
    ((PUBLIC_ORIGIN_TOOLS.contains(&name) && public_origins_grant(context).is_some())
        || (record.employee_scope() && (employee_service_scope(policy) || policy.route == Route::Connector)) || policy.required.iter().all(|kind| record.io_resources()
            .any(|grant| ResourceKind::of(grant) == *kind && validate_resource(grant).is_ok()
                && (*kind != ResourceKind::Employee || grant == &own_employee(context)))))
        && (!OWN_SKILL_TOOLS.contains(&name) || own_skill_selector(context, name).is_ok())
}

fn required_for_call(policy: &ToolPolicy, arguments: &Value) -> Result<Vec<ResourceKind>, VerificationError> {
    let object = arguments.as_object().ok_or(VerificationError::Invalid)?;
    let mut required = policy.required.to_vec();
    let mut add = |kind| { if !required.contains(&kind) { required.push(kind); } };
    for key in ["path", "source_path", "file", "scope_dir", "page_path", "output_path", "directory", "folder"] {
        if let Some(value) = object.get(key) {
            let path = value.as_str().ok_or(VerificationError::Invalid)?;
            crate::resources::relative_path(path)?;
            if !(policy.name == "wiki_read" && key == "page_path") { add(ResourceKind::Workspace); }
        }
    }
    for key in ["url", "url_or_path"] {
        if let Some(value) = object.get(key) {
            let value = value.as_str().ok_or(VerificationError::Invalid)?;
            if key == "url_or_path" && !matches!(value.split_once("://"), Some(("https" | "http", _))) {
                crate::resources::relative_path(value)?;
                add(ResourceKind::Workspace);
            } else {
                if value.is_empty() || value.len() > 8192 || value.chars().any(char::is_control) {
                    return Err(VerificationError::Invalid);
                }
                add(ResourceKind::UrlOrigin);
            }
        }
    }
    for key in ["to", "cc", "bcc", "attendees", "chat_id", "notify_chat_id", "target_agent", "recipient"] {
        if object.get(key).is_some_and(|value| !value.is_null()) { add(ResourceKind::Recipient); }
    }
    if let Some(attachments) = object.get("attachments") {
        for attachment in attachments.as_array().ok_or(VerificationError::Invalid)? {
            let path = attachment.as_str().or_else(|| attachment.get("path").and_then(Value::as_str))
                .ok_or(VerificationError::Invalid)?;
            crate::resources::relative_path(path)?;
            add(ResourceKind::Workspace);
        }
    }
    if object.contains_key("drive_file_id") { add(ResourceKind::Connector); }
    if object.get("write_wiki").and_then(Value::as_bool) == Some(true) { add(ResourceKind::Wiki); }
    if policy.name == "create_task" {
        // H8: the task comes only from the verified record; an argument that
        // names one (even null) is refused instead of being interpreted.
        if TASK_SELECTOR_KEYS.iter().any(|key| object.contains_key(*key)) { return Err(VerificationError::Invalid); }
    } else if matches!(policy.name, "tasks_create" | "tasks_update" | "goals_create" | "activity_post") {
        for key in TASK_SELECTOR_KEYS {
            if object.get(key).is_some_and(|value| !value.is_null()) { add(ResourceKind::Task); }
        }
    }
    Ok(required)
}

/// A grant contains only the canonical origin, never credentials, path, query or
/// fragment. URL parsing uses the same WHATWG implementation for grants/calls.
pub fn validate_origin(origin: &str) -> Result<(), VerificationError> {
    let parsed = url::Url::parse(origin).map_err(|_| VerificationError::Invalid)?;
    if !matches!(parsed.scheme(), "https" | "http") || parsed.host_str().is_none()
        || !parsed.username().is_empty() || parsed.password().is_some()
        || parsed.path() != "/" || parsed.query().is_some() || parsed.fragment().is_some()
        || parsed.origin().ascii_serialization() != origin {
        return Err(VerificationError::Invalid);
    }
    Ok(())
}

/// Exact identifiers only: neither issuance nor execution interprets an empty
/// value or `*` as authority for a collection. Collections use finite grants.
pub fn validate_resource(resource: &ResourceGrant) -> Result<(), VerificationError> {
    let exact = |value: &str| !value.is_empty() && value != "*" && !value.chars().any(char::is_control);
    let valid = match resource {
        ResourceGrant::Confined { resource } => !matches!(resource.as_ref(), ResourceGrant::Confined { .. }
            | ResourceGrant::EmployeeScope { .. } | ResourceGrant::Employee { .. } | ResourceGrant::PublicOrigins { .. }
            | ResourceGrant::NewTask { .. })
            && validate_resource(resource).is_ok(),
        // The requester/principal and employee-scope rules are
        // `CapabilityRecord::check_new_task`; here only an exact identifier.
        ResourceGrant::NewTask { requester } => exact(requester.as_str()),
        // Entries are validated by construction/decode (`PublicOrigin::parse`);
        // the set and employee-scope rules by `validate_resource_mode`. `*` here
        // is the reviewed D-H2 public-origin authority, not a collection grant.
        ResourceGrant::PublicOrigins { origins } => !origins.as_slice().is_empty(),
        ResourceGrant::EmployeeScope { .. } | ResourceGrant::Employee { .. } => true,
        ResourceGrant::Workspace { handle } => exact(handle.as_str()),
        ResourceGrant::Mailbox { id } | ResourceGrant::Task { id } => exact(id.as_str()),
        ResourceGrant::DatabaseTable { database, table } => exact(database.as_str()) && exact(table.as_str()),
        ResourceGrant::Wiki { namespace } => exact(namespace.as_str()),
        ResourceGrant::Connector { account, .. } => exact(account.as_str()),
        ResourceGrant::Recipient { id } => exact(id.as_str()),
        ResourceGrant::UrlOrigin { origin } => validate_origin(origin.as_str()).is_ok(),
    };
    if valid { Ok(()) } else { Err(VerificationError::Invalid) }
}

fn url_origin(value: &str) -> Result<String, VerificationError> {
    if value.len() > 8192 || value.chars().any(char::is_control) {
        return Err(VerificationError::Invalid);
    }
    let parsed = url::Url::parse(value).map_err(|_| VerificationError::Invalid)?;
    if !matches!(parsed.scheme(), "https" | "http") || parsed.host_str().is_none()
        || !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(VerificationError::Invalid);
    }
    let origin = parsed.origin().ascii_serialization();
    validate_origin(&origin)?;
    Ok(origin)
}

fn check_selected_resources(policy: &ToolPolicy, arguments: &Value, resources: &[ResourceGrant])
    -> Result<(), VerificationError> {
    let string = |key| arguments.get(key).map(|value| value.as_str().ok_or(VerificationError::Invalid)).transpose();
    if let Some(target) = string("target_employee")? {
        if target.len() != 32 || !target.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
            return Err(VerificationError::Invalid);
        }
        let mut id = [0; 16];
        for (index, pair) in target.as_bytes().chunks_exact(2).enumerate() {
            let digit = |b: u8| if b.is_ascii_digit() { b - b'0' } else { b - b'a' + 10 };
            id[index] = digit(pair[0]) * 16 + digit(pair[1]);
        }
        if !resources.contains(&ResourceGrant::Employee { id: duduclaw_isolation::ids::EmployeeId(id) }) {
            return Err(VerificationError::Invalid);
        }
    }
    for key in ["workspace", "workspace_id"] {
        if let Some(handle) = string(key)? {
            // A new workspace is created under the employee, never interpreted
            // as an arbitrary host directory or an existing wildcard handle.
            if !(policy.name == "computer_session_start" && handle == "new")
                && !resources.iter().any(|resource| matches!(resource, ResourceGrant::Workspace { handle: allowed } if allowed.as_str() == handle)) {
                return Err(VerificationError::Invalid);
            }
        }
    }
    for key in ["url", "url_or_path"] {
        if let Some(value) = string(key)? {
            if key == "url_or_path" && !matches!(value.split_once("://"), Some(("https" | "http", _))) { continue; }
            let origin = url_origin(value)?;
            if !resources.iter().any(|resource| matches!(resource, ResourceGrant::UrlOrigin { origin: allowed } if allowed.as_str() == origin)) {
                return Err(VerificationError::Invalid);
            }
        }
    }
    if let Some(namespace) = string("namespace")? {
        if !resources.iter().any(|resource| matches!(resource, ResourceGrant::Wiki { namespace: allowed } if allowed.as_str() == namespace)) {
            return Err(VerificationError::Invalid);
        }
    }
    if matches!(policy.name, "db_select" | "db_tables" | "db_query") {
        let database = string("source")?.ok_or(VerificationError::Invalid)?;
        let table = if policy.name == "db_select" { Some(string("table")?.ok_or(VerificationError::Invalid)?) } else { None };
        if !resources.iter().any(|resource| matches!(resource, ResourceGrant::DatabaseTable { database: allowed_db, table: allowed_table }
            if allowed_db.as_str() == database && table.is_none_or(|table| allowed_table.as_str() == table))) {
            return Err(VerificationError::Invalid);
        }
    }
    if let Some(account) = string("connector_account")? {
        if !resources.iter().any(|resource| matches!(resource,
            ResourceGrant::Connector { account: allowed, .. } if allowed.as_str() == account)) {
            return Err(VerificationError::BindingMismatch);
        }
    }
    for key in ["to", "cc", "bcc", "attendees", "chat_id", "notify_chat_id", "target_agent", "recipient"] {
        if let Some(value) = arguments.get(key).filter(|v| !v.is_null()) {
            let values: Vec<&Value> = match value.as_array() { Some(array) => array.iter().collect(), None => vec![value] };
            for value in values {
                let recipient = value.as_str().or_else(|| value.get("email").and_then(Value::as_str))
                    .ok_or(VerificationError::Invalid)?;
                if !resources.iter().any(|resource| matches!(resource,
                    ResourceGrant::Recipient { id } if id.as_str() == recipient)) {
                    return Err(VerificationError::Invalid);
                }
            }
        }
    }
    if policy.route == Route::Service {
        for key in ["task_id", "parent_task_id", "goal_id", "parent_goal_id"] {
            if let Some(task) = string(key)? {
                if !resources.iter().any(|resource| matches!(resource, ResourceGrant::Task { id } if id.as_str() == task)) {
                    return Err(VerificationError::Invalid);
                }
            }
        }
        if let Some(dependencies) = arguments.get("depends_on") {
            for dependency in dependencies.as_array().ok_or(VerificationError::Invalid)? {
                let task = dependency.as_str().ok_or(VerificationError::Invalid)?;
                if !resources.iter().any(|resource| matches!(resource, ResourceGrant::Task { id } if id.as_str() == task)) {
                    return Err(VerificationError::Invalid);
                }
            }
        }
    }
    Ok(())
}

/// D-H2 call path for [`PUBLIC_ORIGIN_TOOLS`]. The selected resources are
/// exactly the record's own signed EmployeeScope and PublicOrigins grants: no
/// resolver is consulted, so host resolution can neither widen nor substitute
/// the origin. Only the declared tool arguments are accepted, and the URL must
/// resolve (same WHATWG parser as the fetcher) to an origin the grant permits.
fn authorize_public_origin(context: &RequestContext, policy: &'static ToolPolicy, arguments: &Value,
    grant: &ResourceGrant) -> Result<AuthorizedToolCall, VerificationError> {
    let object = arguments.as_object().ok_or(VerificationError::Invalid)?;
    let allowed: &[&str] = if policy.name == "web_extract" { &["url", "ttl_seconds", "selector", "format"] }
        else { &["url", "ttl_seconds"] };
    if object.keys().any(|key| !allowed.contains(&key.as_str())) { return Err(VerificationError::Invalid); }
    let url = object.get("url").and_then(Value::as_str).ok_or(VerificationError::Invalid)?;
    crate::resources::permitted_public_origin(grant, url)?;
    let scope = ResourceGrant::EmployeeScope { id: context.record().employee };
    crate::resources::require_resource(context, &scope)?;
    crate::resources::require_resource(context, grant)?;
    Ok(AuthorizedToolCall { context: context.clone(), policy, arguments: arguments.clone(),
        resources: vec![scope, grant.clone()] })
}

/// H9 owner decision 3: `create_task` in an interactive run. Only a new
/// top-level plan: the arguments are exactly the plan (`goal`, `steps`), so no
/// parent task, goal, dependency, assignee or creator can be selected; every
/// step keeps the existing step schema. The resolver must vouch for exactly the
/// own Employee grant and the record's signed `NewTask` grant (it checks the
/// step targets and the live binding); the requester is the signed principal.
fn authorize_new_task(context: &RequestContext, policy: &'static ToolPolicy, arguments: &Value,
    grant: ResourceGrant, resolver: &dyn ResourceResolver) -> Result<AuthorizedToolCall, VerificationError> {
    let object = arguments.as_object().ok_or(VerificationError::Invalid)?;
    if object.keys().any(|key| !NEW_TASK_KEYS.contains(&key.as_str())) { return Err(VerificationError::Invalid); }
    if !object.get("goal").and_then(Value::as_str).is_some_and(|goal| !goal.is_empty()) {
        return Err(VerificationError::Invalid);
    }
    for step in new_task_steps(arguments)? {
        let step = step.as_object().ok_or(VerificationError::Invalid)?;
        if step.keys().any(|key| !NEW_TASK_STEP_KEYS.contains(&key.as_str())) { return Err(VerificationError::Invalid); }
    }
    let employee = own_employee(context);
    let resources = resolver.resolve(context, policy, arguments)?;
    if resources.len() != 2 || !resources.contains(&employee) || !resources.contains(&grant) {
        return Err(VerificationError::Invalid);
    }
    for resource in [&employee, &grant] {
        crate::resources::require_resource(context, resource)?;
        validate_resource(resource)?;
    }
    Ok(AuthorizedToolCall { context: context.clone(), policy, arguments: arguments.clone(),
        resources: vec![employee, grant] })
}

/// Async callers resolve live resources on the bounded authority worker pool.
/// The returned call owns its context, arguments and exact finite resources.
pub async fn authorize_async(context: &RequestContext, name: &str, arguments: &Value,
    resolver: std::sync::Arc<dyn ResourceResolver>) -> Result<AuthorizedToolCall, VerificationError> {
    let (context, name, arguments) = (context.clone(), name.to_owned(), arguments.clone());
    crate::authority::verify_blocking(move || authorize(&context, &name, &arguments, resolver.as_ref())).await
}

pub fn authorize(context: &RequestContext, name: &str, arguments: &Value, resolver: &dyn ResourceResolver)
    -> Result<AuthorizedToolCall, VerificationError> {
    let policy = policy(name).ok_or(VerificationError::Invalid)?;
    // Service selectors remain data under the original schema.
    // H12a: own-skill tools accept only their exact protected schema; the
    // curator's `run` is a typed control flag, not an execution identity.
    if OWN_SKILL_TOOLS.contains(&name) {
        check_own_skill_arguments(name, arguments)?;
    } else if context.record().employee_scope() && matches!(policy.route, Route::Service | Route::Connector) {
        let fields = arguments.as_object().ok_or(VerificationError::Invalid)?;
        if ["principal", "root", "cwd", "run", "run_id", "employee_id"].iter().any(|k| fields.contains_key(*k)) {
            return Err(VerificationError::Invalid);
        }
    } else { crate::check_arguments(arguments)?; }
    if !eligible(context, name) { return Err(VerificationError::Invalid); }
    if PUBLIC_ORIGIN_TOOLS.contains(&name) {
        if let Some(grant) = public_origins_grant(context) {
            return authorize_public_origin(context, policy, arguments, grant);
        }
    }
    if name == "create_task" && context.record().task.is_none() {
        // No record task: only the interactive NewTask path exists.
        return authorize_new_task(context, policy, arguments, new_task_grant(context)?, resolver);
    }
    if context.record().employee_scope() && employee_service_scope(policy) {
        return Ok(AuthorizedToolCall { context: context.clone(), policy, arguments: arguments.clone(),
            resources: vec![ResourceGrant::EmployeeScope { id: context.record().employee }] });
    }
    let employee_connector = context.record().employee_scope() && policy.route == Route::Connector;
    let required = if employee_connector { vec![ResourceKind::Employee, ResourceKind::Connector] }
        else { required_for_call(policy, arguments)? };
    let resources = resolver.resolve(context, policy, arguments)?;
    if resources.is_empty() || resources.len() > 128
        || resources.iter().enumerate().any(|(index, resource)| resources[..index].contains(resource)) {
        return Err(VerificationError::Invalid);
    }
    for resource in &resources {
        if !employee_connector { crate::resources::require_resource(context, resource)?; }
        if let ResourceGrant::Connector { binding, .. } = resource {
            // A shared home account may be selected only by the live product
            // policy; personal credentials still require the lifetime owner.
            if binding != &context.record().employee_binding
                && !matches!((&context.record().employee_binding, binding), (EmployeeBinding::Personal(_), EmployeeBinding::Shared(_))) {
                return Err(VerificationError::BindingMismatch);
            }
            if let EmployeeBinding::Personal(owner) = binding {
                if !context.record().personal_owner_matches(owner.as_str()) {
                    return Err(VerificationError::BindingMismatch);
                }
            }
        }
        validate_resource(resource)?;
    }
    if !required.iter().all(|kind| resources.iter().any(|resource| ResourceKind::of(resource) == *kind
        && (*kind != ResourceKind::Employee || resource == &own_employee(context)))) {
        return Err(VerificationError::Invalid);
    }
    if employee_connector {
        if let Some(account) = arguments.get("connector_account") {
            let account = account.as_str().ok_or(VerificationError::Invalid)?;
            if !resources.iter().any(|r| matches!(r, ResourceGrant::Connector { account: selected, .. } if selected.as_str() == account)) {
                return Err(VerificationError::BindingMismatch);
            }
        }
    } else { check_selected_resources(policy, arguments, &resources)?; }
    if name == "create_task" {
        // Exactly the record's own task, once; the resolver vouched for live
        // ownership and cannot substitute or add another task.
        let selector = create_task_selector(context)?;
        if !resources.iter().filter(|resource| ResourceKind::of(resource) == ResourceKind::Task).eq([&selector]) {
            return Err(VerificationError::Invalid);
        }
    }
    if OWN_SKILL_TOOLS.contains(&name) {
        // H12a: exactly the record's own selection. The resolver vouched for
        // live identity and the own wiki namespace; it cannot add a workspace,
        // a recipient (another employee) or a second namespace.
        let selection = own_skill_selector(context, name)?;
        if resources.len() != selection.len() || !selection.iter().all(|grant| resources.contains(grant)) {
            return Err(VerificationError::Invalid);
        }
    }
    Ok(AuthorizedToolCall { context: context.clone(), policy, arguments: arguments.clone(), resources })
}

#[cfg(test)]
#[path = "tool_policy_tests.rs"]
mod tests;
