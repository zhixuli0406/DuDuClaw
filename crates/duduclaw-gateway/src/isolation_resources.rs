//! Live resource resolution for the gateway's isolated MCP adapter.
//! The employee/agent mapping is supplied by the authenticated boot registry.
use std::{path::{Path, PathBuf}, sync::Arc};
use duduclaw_isolation::{bounded::Text, capability::ResourceGrant, ids::EmployeeId};
use duduclaw_mcp_boundary::{bindings::BindingAuthority, tool_policy::{ResourceKind, ResourceResolver, ToolPolicy},
    RequestContext, VerificationError};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde_json::Value;
#[cfg(test)]
use std::sync::Mutex;
#[cfg(test)]
use duduclaw_mcp_boundary::bindings::BindingStore;
use crate::isolation_connectors::{ConnectorCatalog, LiveConnectorSelectionPolicy, SelectedConnector};

pub struct GatewayResourceResolver {
    home: PathBuf,
    employee: EmployeeId,
    agent: String,
    bindings: Arc<dyn BindingAuthority>,
    connectors: Option<Arc<ConnectorCatalog>>,
    connector_policy: Option<Arc<dyn LiveConnectorSelectionPolicy>>,
}
impl GatewayResourceResolver {
    pub fn new(home: PathBuf, employee: EmployeeId, agent: String,
        bindings: Arc<dyn BindingAuthority>) -> Result<Self, VerificationError> {
        if !duduclaw_core::is_valid_agent_id(&agent) || duduclaw_core::is_reserved_agent_id(&agent)
            || agent == duduclaw_core::UNTRUSTED_AGENT_ID || !home.is_absolute() {
            return Err(VerificationError::Invalid);
        }
        Ok(Self { home, employee, agent, bindings, connectors: None, connector_policy: None })
    }
    pub fn with_connector_catalog(mut self, catalog: Arc<ConnectorCatalog>) -> Self {
        self.connectors = Some(catalog); self
    }
    pub fn with_connector_selection_policy(mut self, policy: Arc<dyn LiveConnectorSelectionPolicy>) -> Self {
        self.connector_policy = Some(policy); self
    }
    pub fn connector_selection_ready(&self, tool: &str) -> bool {
        self.connectors.is_some() && self.connector_policy.as_ref().is_some_and(|policy| policy.supports(tool))
    }
    pub fn connector(&self, context: &RequestContext, tool: &str, arguments: &Value)
        -> Result<SelectedConnector, VerificationError> {
        self.identity(context)?;
        let policy = self.connector_policy.clone().ok_or(VerificationError::Unavailable)?;
        SelectedConnector::select_with_policy(context, tool, arguments, self.bindings.clone(),
            self.connectors.clone().ok_or(VerificationError::Unavailable)?, Some(policy))
    }
    fn identity(&self, context: &RequestContext) -> Result<(), VerificationError> {
        let record = context.record();
        if record.employee != self.employee { return Err(VerificationError::BindingMismatch); }
        self.bindings.validate(record.employee, &record.employee_binding, &record.principal)
            .map_err(|_| VerificationError::BindingMismatch)
    }
    fn database(&self, filename: &str) -> Result<Connection, VerificationError> {
        let connection = Connection::open_with_flags(self.home.join(filename), OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|_| VerificationError::Unavailable)?;
        connection.busy_timeout(std::time::Duration::from_millis(500))
            .map_err(|_| VerificationError::Unavailable)?;
        Ok(connection)
    }
    fn task_owned(&self, tool: &str, id: &str) -> Result<bool, VerificationError> {
        let (database, query, key) = match task_domain(tool) {
            "cron" => ("cron_tasks.db", "SELECT 1 FROM cron_tasks WHERE id=?1 AND agent_id=?2", id.strip_prefix("cron:")),
            "plan" => ("tasks.db", "SELECT 1 FROM plans WHERE id=?1 AND agent_id=?2", id.strip_prefix("plan:")),
            "responsibility" => ("tasks.db", "SELECT 1 FROM responsibilities WHERE responsibility_id=?1 AND owner_agent_id=?2", id.strip_prefix("responsibility:")),
            "reminder" => return reminder_owned(&self.home, &self.agent, id),
            _ => ("tasks.db", "SELECT 1 FROM tasks WHERE id=?1 AND assigned_to=?2", Some(id)),
        };
        let Some(key) = key else { return Ok(false); };
        let found = self.database(database)?.query_row(query, params![key, self.agent], |_| Ok(()))
            .optional().map_err(|_| VerificationError::Unavailable)?.is_some();
        if found && task_domain(tool) == "task" {
            // Existing audience semantics are preserved, including the owner
            // exception; unreadable packets never grant a non-owner access.
            let audience = crate::review_evidence::audience::task_audience(&self.home, key);
            return Ok(crate::review_evidence::audience::agent_may_read_task(&self.agent, &[&self.agent], &audience));
        }
        Ok(found)
    }
    /// H8 live task selector for `create_task`. The task is the run's own task
    /// from the verified record (never an argument; selector arguments are
    /// refused), it must carry the exact signed grant, and the task store must
    /// say NOW that this employee's agent owns it (`tasks.assigned_to` + the
    /// existing audience rule). Re-read on every call: no caching, so a task
    /// reassigned or deleted mid-run stops authorizing new plans immediately.
    fn create_task_selector(&self, context: &RequestContext, args: &Value)
        -> Result<Vec<ResourceGrant>, VerificationError> {
        if duduclaw_mcp_boundary::tool_policy::TASK_SELECTOR_KEYS.iter().any(|key| args.get(*key).is_some()) {
            return Err(VerificationError::Invalid);
        }
        let selector = duduclaw_mcp_boundary::tool_policy::create_task_selector(context)?;
        let ResourceGrant::Task { id } = &selector else { return Err(VerificationError::Invalid); };
        if !self.task_owned("create_task", id.as_str())? { return Err(VerificationError::Invalid); }
        Ok(vec![selector])
    }
    /// H9 owner decision 3: `create_task` of an interactive run files a NEW
    /// top-level plan only. Fixed fields, none from arguments: the assignee is
    /// this employee (every step targets this employee's own agent or names
    /// none, which the handler reads as the caller), and the creator/audience
    /// is the signed requester, which must be the record's human principal
    /// (`new_task_grant`) and, for a Personal employee, its owner (B1). Task,
    /// goal and dependency selectors are refused; nothing is read from tasks.db.
    fn new_task_selector(&self, context: &RequestContext, args: &Value)
        -> Result<Vec<ResourceGrant>, VerificationError> {
        use duduclaw_mcp_boundary::tool_policy::{new_task_grant, new_task_steps, TASK_SELECTOR_KEYS};
        if TASK_SELECTOR_KEYS.iter().any(|key| args.get(*key).is_some()) { return Err(VerificationError::Invalid); }
        let grant = new_task_grant(context)?;
        let ResourceGrant::NewTask { requester } = &grant else { return Err(VerificationError::Invalid); };
        let record = context.record();
        if let duduclaw_isolation::capability::EmployeeBinding::Personal(_) = &record.employee_binding {
            if !record.personal_owner_matches(requester.as_str()) { return Err(VerificationError::BindingMismatch); }
        }
        for step in new_task_steps(args)? {
            for key in ["agent", "agent_id", "assigned_to", "assignee"] {
                match step.get(key) {
                    None | Some(Value::Null) => {}
                    Some(Value::String(agent)) if agent.is_empty() || *agent == self.agent => {}
                    Some(_) => return Err(VerificationError::BindingMismatch),
                }
            }
        }
        Ok(vec![grant])
    }
    fn selected_tasks(&self, context: &RequestContext, policy: &ToolPolicy, args: &Value)
        -> Result<Vec<ResourceGrant>, VerificationError> {
        if policy.name == "create_task" {
            // A task/schedule run plans under its own record task (H8); an
            // interactive run (no record task) may only file a new plan.
            return if context.record().task.is_some() { self.create_task_selector(context, args) }
                else { self.new_task_selector(context, args) };
        }
        let mut selected = Vec::new();
        for key in ["task_id", "parent_task_id", "goal_id", "parent_goal_id"] {
            if let Some(id) = string(args, key)? { selected.push(id.to_owned()); }
        }
        if let Some(ids) = args.get("depends_on") {
            for id in ids.as_array().ok_or(VerificationError::Invalid)? {
                selected.push(id.as_str().ok_or(VerificationError::Invalid)?.to_owned());
            }
        }
        let domain = task_domain(policy.name);
        let selector = match domain { "cron" | "reminder" => "id", "plan" => "plan_id",
            "responsibility" => "responsibility_id", _ => "task_id" };
        if domain != "task" {
            if let Some(id) = string(args, selector)? {
                let id = if policy.name == "responsibility_get" { id.trim() } else { id };
                if policy.name != "responsibility_get" || !id.is_empty() {
                    selected.push(format!("{domain}:{id}"));
                }
            }
        }
        if policy.name == "plan_update_step" {
            let step = string(args, "step_id")?.ok_or(VerificationError::Invalid)?;
            let id: String = self.database("tasks.db")?.query_row(
                "SELECT s.plan_id FROM plan_steps s JOIN plans p ON p.id=s.plan_id
                 WHERE s.id=?1 AND s.assignee_kind='agent' AND s.assignee=?2 AND p.agent_id=?2",
                params![step, self.agent], |row| row.get(0)).map_err(|_| VerificationError::Invalid)?;
            selected.push(format!("plan:{id}"));
        }
        // Name-based cron selectors are mutable and ambiguous. Isolated calls
        // use the immutable id; the public legacy handler remains unchanged.
        if domain == "cron" && policy.name != "list_cron_tasks" && string(args, "id")?.is_none() {
            return Err(VerificationError::Invalid);
        }
        if selected.is_empty() && matches!(policy.name, "capability_request" | "responsibility_followup" | "responsibility_ask") {
            if let Some(task) = &context.record().task { selected.push(task.as_str().to_owned()); }
        }
        // Legacy defaults re-query the latest plan or all responsibilities.
        // A signed finite grant can only authorize an explicit immutable id.
        if matches!(policy.name, "plan_get")
            && string(args, selector)?.is_none_or(str::is_empty) {
            return Err(VerificationError::Invalid);
        }
        let collection = matches!(policy.name, "tasks_list" | "activity_list" | "goals_list" | "list_cron_tasks" | "list_reminders" | "responsibility_get");
        let mut resources = Vec::new();
        if selected.is_empty() && !collection { return Err(VerificationError::Invalid); }
        for grant in context.record().io_resources() {
            if let ResourceGrant::Task { id } = grant {
                if (selected.is_empty() || selected.iter().any(|s| s == id.as_str())) && self.task_owned(policy.name, id.as_str())? {
                    resources.push(grant.clone());
                }
            }
        }
        for id in selected {
            if !resources.iter().any(|r| matches!(r, ResourceGrant::Task { id: found } if found.as_str() == id)) {
                return Err(VerificationError::Invalid);
            }
        }
        Ok(resources)
    }
}

fn text<const N: usize>(value: String) -> Result<Text<N>, VerificationError> {
    Text::new(value).map_err(|_| VerificationError::Invalid)
}
fn string<'a>(args: &'a Value, key: &str) -> Result<Option<&'a str>, VerificationError> {
    args.get(key).filter(|v| !v.is_null()).map(|v| v.as_str().ok_or(VerificationError::Invalid)).transpose()
}
fn task_domain(tool: &str) -> &'static str {
    match tool {
        "list_cron_tasks" | "update_cron_task" | "delete_cron_task" | "pause_cron_task" | "run_cron_task" => "cron",
        "list_reminders" | "cancel_reminder" => "reminder",
        "plan_get" | "plan_update_step" => "plan",
        "responsibility_get" | "responsibility_followup" | "responsibility_ask" => "responsibility",
        _ => "task",
    }
}
fn reminder_owned(home: &Path, agent: &str, id: &str) -> Result<bool, VerificationError> {
    use std::io::Read;
    let Some(id) = id.strip_prefix("reminder:") else { return Ok(false); };
    let file = std::fs::File::open(home.join("reminders.jsonl")).map_err(|_| VerificationError::Unavailable)?;
    if file.metadata().map_err(|_| VerificationError::Unavailable)?.len() > 8 * 1024 * 1024 {
        return Err(VerificationError::Unavailable);
    }
    let mut content = String::new();
    file.take(8 * 1024 * 1024 + 1).read_to_string(&mut content).map_err(|_| VerificationError::Unavailable)?;
    if content.len() > 8 * 1024 * 1024 { return Err(VerificationError::Unavailable); }
    let mut found = false;
    for (index, line) in content.lines().enumerate() {
        if index >= 10000 { return Err(VerificationError::Unavailable); }
        if line.trim().is_empty() { continue; }
        let row: Value = serde_json::from_str(&line).map_err(|_| VerificationError::Unavailable)?;
        if row["id"].as_str() == Some(id) {
            if found || row["agent_id"].as_str() != Some(agent) { return Err(VerificationError::Invalid); }
            found = true;
        }
    }
    Ok(found)
}

/// Local database grants name the employee namespace, never a host DB path.
fn local_table(tool: &str) -> Option<&'static str> {
    Some(match tool {
        "working_state_set" | "working_state_clear" | "working_state_handoff" | "working_state_get" => "working_state",
        "belief_submit" | "belief_settle" | "belief_stats" => "beliefs",
        "user_profile_record" | "user_profile_get" | "user_code_profile" => "user_profile",
        "decision_list" | "decision_resolve" => "decisions",
        "memory_search" | "memory_store" | "memory_read" | "memory_fetch_batch" | "memory_alias_add" |
        "memory_alias_list" | "memory_get_history" | "memory_get_at" | "memory_invalidate_by_origin" |
        "memory_search_by_layer" | "memory_successful_conversations" | "memory_episodic_pressure" |
        "memory_consolidation_status" => "memory",
        _ => return None,
    })
}

impl ResourceResolver for GatewayResourceResolver {
    fn resolve(&self, context: &RequestContext, policy: &ToolPolicy, args: &Value)
        -> Result<Vec<ResourceGrant>, VerificationError> {
        self.identity(context)?;
        let object = args.as_object().ok_or(VerificationError::Invalid)?;
        if context.record().employee_scope() && policy.route == duduclaw_mcp_boundary::tool_policy::Route::Connector {
            let selected = self.connector(context, policy.name, args)?;
            return Ok(vec![ResourceGrant::Employee { id: self.employee }, ResourceGrant::Connector {
                account: text(selected.account().to_owned())?, binding: selected.binding().clone() }]);
        }
        if let Some(value) = object.get("assigned_to") {
            let assigned = value.as_str().ok_or(VerificationError::Invalid)?;
            if assigned != self.agent && !(policy.name == "tasks_list" && assigned == "*") {
                return Err(VerificationError::BindingMismatch);
            }
        }
        if policy.name == "tasks_create" {
            if let Some(value) = object.get("kind") {
                if !matches!(value.as_str(), Some("task" | "goal")) { return Err(VerificationError::Invalid); }
            }
        }
        let wants = |kind| policy.required.contains(&kind);
        let mut resources = if wants(ResourceKind::Employee) { vec![ResourceGrant::Employee { id: self.employee }] } else { vec![] };
        if wants(ResourceKind::Task) || ["task_id", "parent_task_id", "goal_id", "parent_goal_id", "depends_on"]
            .iter().any(|key| object.contains_key(*key)) {
            resources.extend(self.selected_tasks(context, policy, args)?);
        }
        if wants(ResourceKind::Connector) || object.contains_key("drive_file_id") {
            let selected = self.connector(context, policy.name, args)?;
            resources.push(ResourceGrant::Connector { account: text(selected.account().to_owned())?,
                binding: selected.binding().clone() });
        }
        if wants(ResourceKind::DatabaseTable) {
            if let Some(table) = local_table(policy.name) {
                resources.push(ResourceGrant::DatabaseTable { database: text(format!("agent:{}", self.agent))?, table: text(table.into())? });
            } else {
                let source = string(args, "source")?;
                let table = string(args, "table")?;
                for grant in context.record().io_resources() {
                    if let ResourceGrant::DatabaseTable { database, table: allowed } = grant {
                        if source.is_none_or(|s| database.as_str() == s) && table.is_none_or(|t| allowed.as_str() == t) {
                            resources.push(grant.clone());
                        }
                    }
                }
            }
        }
        if wants(ResourceKind::Wiki) || args.get("write_wiki").and_then(Value::as_bool) == Some(true) {
            let namespace = match string(args, "scope")?.unwrap_or("agent") {
                "agent" => format!("agent:{}", self.agent), "shared" => "shared".to_owned(),
                _ => return Err(VerificationError::Invalid),
            };
            if string(args, "namespace")?.is_some_and(|n| n != namespace) { return Err(VerificationError::Invalid); }
            resources.push(ResourceGrant::Wiki { namespace: text(namespace)? });
        }
        if wants(ResourceKind::Mailbox) {
            let mailbox = if let Some(connector) = resources.iter().find_map(|r| match r {
                ResourceGrant::Connector { account, .. } => Some(account.as_str()), _ => None,
            }) { format!("account:{connector}") } else { format!("agent:{}", self.agent) };
            resources.push(ResourceGrant::Mailbox { id: text(mailbox)? });
        }
        for key in ["to", "cc", "bcc", "attendees", "chat_id", "notify_chat_id", "target_agent", "recipient"] {
            if let Some(value) = args.get(key).filter(|v| !v.is_null()) {
                let values = if let Some(values) = value.as_array() { values.clone() } else { vec![value.clone()] };
                for value in values {
                    let recipient = value.as_str().or_else(|| value.get("email").and_then(Value::as_str))
                        .ok_or(VerificationError::Invalid)?;
                    // Do not split comma-delimited strings: provider adapters must
                    // receive exactly one explicitly granted destination per item.
                    if recipient.contains(',') || recipient.contains(';') || recipient.trim() != recipient {
                        return Err(VerificationError::Invalid);
                    }
                    let grant = ResourceGrant::Recipient { id: text(recipient.into())? };
                    if !resources.contains(&grant) { resources.push(grant); }
                }
            }
        }
        let workspace = string(args, "workspace")?.or(string(args, "workspace_id")?);
        if wants(ResourceKind::Workspace) || workspace.is_some() || ["path", "file", "source_path", "scope_dir", "page_path", "output_path", "directory", "folder", "attachments"]
            .iter().any(|key| object.contains_key(*key) && !(policy.name == "wiki_read" && *key == "page_path")) {
            for grant in context.record().io_resources() {
                if let ResourceGrant::Workspace { handle } = grant {
                    if workspace.is_none_or(|w| w == handle.as_str()) { resources.push(grant.clone()); }
                }
            }
        }
        // HX6: the recording's navigation URL is authorized by the record's
        // signed PublicOrigins grant, which the boundary itself selects
        // (`tool_policy::RECORDING_URL_TOOLS`); no per-URL UrlOrigin here.
        let recording = duduclaw_mcp_boundary::tool_policy::RECORDING_URL_TOOLS.contains(&policy.name);
        for key in ["url", "url_or_path"] {
            if recording { break; }
            if let Some(value) = string(args, key)? {
                if key == "url_or_path" && !matches!(value.split_once("://"), Some(("https" | "http", _))) { continue; }
                let parsed = url::Url::parse(value).map_err(|_| VerificationError::Invalid)?;
                if !parsed.username().is_empty() || parsed.password().is_some() { return Err(VerificationError::Invalid); }
                resources.push(ResourceGrant::UrlOrigin { origin: text(parsed.origin().ascii_serialization())? });
            }
        }
        // The boundary checks exact signed inclusion, resource kind completeness,
        // selectors and duplicates; the resolver does not silently widen grants.
        Ok(resources)
    }
}

#[cfg(test)]
#[path = "isolation_resources_tests.rs"]
mod tests;
