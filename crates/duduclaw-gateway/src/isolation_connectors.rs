//! Account selection for isolated MCP. No credential fallback or ambient account.
use std::{future::Future, pin::Pin, sync::{Arc, Mutex}};
use duduclaw_isolation::capability::{EmployeeBinding, ResourceGrant};
use duduclaw_mcp_boundary::{bindings::BindingAuthority, tool_policy::AuthorizedToolCall,
    RequestContext, VerificationError};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;
#[cfg(test)]
use duduclaw_mcp_boundary::bindings::BindingStore;

/// Gateway-owned account routing metadata. Registration happens only after the
/// connector's actual account has been provisioned; deletion/revocation is final.
/// This holds no secret and does not import the gateway's legacy home account.
pub struct ConnectorCatalog { connection: Mutex<Connection> }
impl ConnectorCatalog {
    pub fn new(connection: Connection) -> Result<Self, VerificationError> {
        connection.execute_batch("CREATE TABLE IF NOT EXISTS isolation_connector_routes (
            account TEXT PRIMARY KEY NOT NULL, provider TEXT NOT NULL,
            active INTEGER NOT NULL CHECK(active IN (0,1))
        ) STRICT;
        CREATE TRIGGER IF NOT EXISTS isolation_connector_no_replace BEFORE INSERT ON isolation_connector_routes
        WHEN EXISTS(SELECT 1 FROM isolation_connector_routes WHERE account=NEW.account)
        BEGIN SELECT RAISE(ABORT,'immutable account route'); END;
        CREATE TRIGGER IF NOT EXISTS isolation_connector_no_update BEFORE UPDATE ON isolation_connector_routes
        WHEN NEW.account!=OLD.account OR NEW.provider!=OLD.provider OR NEW.active>OLD.active
        BEGIN SELECT RAISE(ABORT,'immutable account route'); END;
        CREATE TRIGGER IF NOT EXISTS isolation_connector_no_delete BEFORE DELETE ON isolation_connector_routes
        BEGIN SELECT RAISE(ABORT,'immutable account route'); END;")
            .map_err(|_| VerificationError::Unavailable)?;
        Ok(Self { connection: Mutex::new(connection) })
    }
    pub fn register(&self, account: &str, provider: &str) -> Result<(), VerificationError> {
        if account.is_empty() || account.len() > 128 || account.chars().any(char::is_control)
            || !matches!(provider, "google" | "notion" | "github" | "odoo" | "database" | "channel" | "search" | "model" | "computer") {
            return Err(VerificationError::Invalid);
        }
        self.connection.lock().map_err(|_| VerificationError::Unavailable)?
            .execute("INSERT INTO isolation_connector_routes VALUES (?1,?2,1)", params![account, provider])
            .map_err(|_| VerificationError::Invalid)?;
        Ok(())
    }
    pub fn revoke(&self, account: &str) -> Result<(), VerificationError> {
        let count = self.connection.lock().map_err(|_| VerificationError::Unavailable)?
            .execute("UPDATE isolation_connector_routes SET active=0 WHERE account=?1", [account])
            .map_err(|_| VerificationError::Unavailable)?;
        if count != 1 { return Err(VerificationError::Invalid); }
        Ok(())
    }
    fn matches(&self, account: &str, provider: &str) -> Result<bool, VerificationError> {
        self.connection.lock().map_err(|_| VerificationError::Unavailable)?
            .query_row("SELECT 1 FROM isolation_connector_routes WHERE account=?1 AND provider=?2 AND active=1",
                params![account, provider], |_| Ok(())).optional()
            .map(|row| row.is_some()).map_err(|_| VerificationError::Unavailable)
    }
}

pub fn provider_for_tool(tool: &str) -> Option<&'static str> {
    Some(match tool {
        "google_status" | "gmail_search" | "gmail_read" | "gmail_create_draft" |
        "google_chat_spaces" | "google_chat_messages" |
        "calendar_list_events" | "calendar_create_event" | "sheets_read" | "sheets_append" |
        "forms_get" | "forms_list_responses" | "gtasks_lists" | "gtasks_list" | "gtasks_create" |
        "gtasks_complete" | "drive_search" | "drive_read" | "docs_read" | "docs_append" | "slides_read" => "google",
        "notion_status" | "notion_search" | "notion_page_read" | "notion_page_append" => "notion",
        "github_status" | "github_search_issues" | "github_issue_read" | "github_pr_read" | "github_issue_comment" => "github",
        "odoo_connect" | "odoo_status" | "odoo_crm_leads" | "odoo_crm_create_lead" | "odoo_crm_update_stage" |
        "odoo_sale_orders" | "odoo_sale_create_quotation" | "odoo_sale_confirm" | "odoo_inventory_products" |
        "odoo_inventory_check" | "odoo_invoice_list" | "odoo_payment_status" | "odoo_search" | "odoo_execute" |
        "odoo_report" | "odoo_partner_search" | "odoo_schema_overview" | "odoo_schema_fields" => "odoo",
        "db_sources" | "db_tables" | "db_select" | "db_query" => "database",
        "group_history_read" | "send_message" | "send_photo" | "send_sticker" |
        "channel_config" | "channel_config_list" | "channel_status" | "pairing_manage" => "channel",
        "web_search" => "search",
        "model_search" | "model_recommend" => "model",
        "computer_login" => "computer",
        _ => return None,
    })
}

/// A concrete current product selection, supplied by a trusted gateway adapter.
/// This is never deserialized from employee arguments. SharedHome must identify
/// the installed home account and its tenant after current per-employee config,
/// integration, requester and consent rules have all been checked.
#[derive(Clone, PartialEq, Eq)]
pub enum ProductConnectorAccount {
    Personal { account: String, owner: String },
    SharedHome { account: String, tenant: String },
}
impl ProductConnectorAccount {
    pub fn account(&self) -> &str {
        match self { Self::Personal { account, .. } | Self::SharedHome { account, .. } => account }
    }
    fn binding(&self) -> Result<EmployeeBinding, VerificationError> {
        use duduclaw_isolation::bounded::Text;
        match self {
            Self::Personal { owner, .. } => Text::new(owner.clone()).map(EmployeeBinding::Personal),
            Self::SharedHome { tenant, .. } => Text::new(tenant.clone()).map(EmployeeBinding::Shared),
        }.map_err(|_| VerificationError::Invalid)
    }
}

/// Implements today's live product authority, not a startup snapshot. Selection
/// is repeated before credentials, retries and effects, so changing configuration
/// cannot keep an earlier home fallback alive. Missing adapters fail closed.
pub trait LiveConnectorSelectionPolicy: Send + Sync {
    fn supports(&self, tool: &str) -> bool;
    fn candidates(&self, context: &RequestContext, tool: &str, provider: &str)
        -> Result<Vec<ProductConnectorAccount>, VerificationError>;
    /// Argument selectors may narrow product-authorized accounts, never add one.
    /// Rechecks receive the exact original arguments retained by selection.
    fn candidates_for_call(&self, context: &RequestContext, tool: &str, provider: &str, _arguments: &Value)
        -> Result<Vec<ProductConnectorAccount>, VerificationError> {
        self.candidates(context, tool, provider)
    }
}

/// Constructible only through selection and live checks. Executors must use
/// this exact account; never select defaults from home, environment or config.
pub struct SelectedConnector {
    account: String,
    provider: &'static str,
    binding: EmployeeBinding,
    tool: String,
    arguments: Value,
    product_selection: (Arc<dyn LiveConnectorSelectionPolicy>, ProductConnectorAccount),
    context: RequestContext,
    bindings: Arc<dyn BindingAuthority>,
    catalog: Arc<ConnectorCatalog>,
}
impl SelectedConnector {
    pub fn account(&self) -> &str { &self.account }
    pub fn provider(&self) -> &'static str { self.provider }
    pub fn binding(&self) -> &EmployeeBinding { &self.binding }
    pub fn context(&self) -> &RequestContext { &self.context }
    /// Carry account/product revocation to every legacy storage or HTTP checkpoint.
    pub fn io_context(self: &Arc<Self>) -> RequestContext {
        struct Selection(Arc<SelectedConnector>);
        impl duduclaw_isolation::capability::LiveRequestVerifier for Selection {
            fn verify_live(&self, _: &duduclaw_isolation::capability::CapabilityRecord, _: u64)
                -> Result<(), VerificationError> { self.0.recheck() }
        }
        RequestContext::with_live_verifier(self.context.record().clone(), Arc::new(Selection(self.clone())))
    }
    /// Only for a sealed source owner that independently checks its original
    /// product proof on every effect (Google). Full selection is checked before
    /// entering and after leaving that owner; nested source locks never reenter
    /// the product selector. Binding/catalog/run revocation is still live.
    pub(crate) fn binding_io_context(self: &Arc<Self>) -> RequestContext {
        struct Binding(Arc<SelectedConnector>);
        impl duduclaw_isolation::capability::LiveRequestVerifier for Binding {
            fn verify_live(&self, _: &duduclaw_isolation::capability::CapabilityRecord, _: u64)
                -> Result<(), VerificationError> { self.0.recheck_binding() }
        }
        RequestContext::with_live_verifier(self.context.record().clone(), Arc::new(Binding(self.clone())))
    }
    /// A directory operation may inspect only separately authorized members.
    /// Each member repeats the full binding/catalog/product selection path.
    pub fn collection_member(&self, account: &str) -> Result<Self, VerificationError> {
        if self.tool != "db_sources" { return Err(VerificationError::Invalid); }
        self.recheck()?;
        let mut arguments = self.arguments.clone();
        arguments.as_object_mut().ok_or(VerificationError::Invalid)?
            .insert("connector_account".into(), Value::String(account.to_owned()));
        Self::select_with_policy(&self.context, &self.tool, &arguments, self.bindings.clone(),
            self.catalog.clone(), Some(self.product_selection.0.clone()))
    }

    pub fn tool(&self) -> &str { &self.tool }
    /// Bind an account selection to the precise authorized operation. Sharing a
    /// provider, record or account never lets a status/read selection authorize
    /// another tool or a different recipient/body/query.
    pub fn verify_call(&self, call: &AuthorizedToolCall) -> Result<(), VerificationError> {
        self.recheck()?;
        let connectors: Vec<_> = call.resources().iter().filter_map(|resource| match resource {
            ResourceGrant::Connector { account, binding } => Some((account.as_str(), binding)),
            _ => None,
        }).collect();
        if call.context().record() != self.context.record() || call.name() != self.tool
            || call.arguments() != &self.arguments || provider_for_tool(call.name()) != Some(self.provider)
            || connectors != [(self.account.as_str(), &self.binding)] {
            return Err(VerificationError::BindingMismatch);
        }
        call.context().verify_live(duduclaw_mcp_boundary::authority::now_seconds()?)
    }
    /// Repeat immediately before credential acquisition and every effect/retry.
    pub fn recheck(&self) -> Result<(), VerificationError> {
        let now = duduclaw_mcp_boundary::authority::now_seconds()?;
        self.context.verify_live(now)?;
        let (policy, selected) = &self.product_selection;
        if !policy.supports(&self.tool) { return Err(VerificationError::Unavailable); }
        let candidates = policy.candidates_for_call(&self.context, &self.tool, self.provider, &self.arguments)?;
        if candidates.len() > 128 || candidates.iter().filter(|candidate| *candidate == selected).count() != 1 {
            return Err(VerificationError::BindingMismatch);
        }
        self.recheck_binding()
    }
    /// Source owners validate their pinned product proof under their own locks.
    /// This checkpoint may run while those locks are held and therefore must
    /// never invoke product selection or reopen the source's metadata store.
    fn recheck_binding(&self) -> Result<(), VerificationError> {
        let record = self.context.record();
        self.context.verify_live(duduclaw_mcp_boundary::authority::now_seconds()?)?;
        let bindings = &self.bindings;
        if self.binding == record.employee_binding {
            bindings.authorize_connector(record.employee, &record.employee_binding, &record.principal, &self.account)
                .map_err(|_| VerificationError::BindingMismatch)?;
        } else if matches!(&self.product_selection.1, ProductConnectorAccount::SharedHome { .. }) {
            bindings.authorize_product_home_connector(record.employee, &record.employee_binding,
                &record.principal, &self.account, &self.binding).map_err(|_| VerificationError::BindingMismatch)?;
        } else { return Err(VerificationError::BindingMismatch); }
        if !self.catalog.matches(&self.account, self.provider)? { return Err(VerificationError::BindingMismatch); }
        // Policy and binding reads may run concurrently with a lifecycle revoke.
        // Make the authority read the final operation before returning to I/O.
        self.context.verify_live(duduclaw_mcp_boundary::authority::now_seconds()?)
    }
    pub fn select_with_policy(context: &RequestContext, tool: &str, arguments: &Value,
        bindings: Arc<dyn BindingAuthority>, catalog: Arc<ConnectorCatalog>,
        product_policy: Option<Arc<dyn LiveConnectorSelectionPolicy>>) -> Result<Self, VerificationError> {
        let provider = provider_for_tool(tool).ok_or(VerificationError::Invalid)?;
        let employee_scope = context.record().employee_scope()
            && duduclaw_mcp_boundary::tool_policy::policy(tool)
                .is_some_and(|policy| policy.route == duduclaw_mcp_boundary::tool_policy::Route::Connector);
        if !context.record().tools.as_slice().iter().any(|allowed| allowed.as_str() == tool) {
            return Err(VerificationError::Invalid);
        }
        let explicit = arguments.get("connector_account").map(|v| v.as_str().ok_or(VerificationError::Invalid)).transpose()?;
        // Credentials cannot be declared/overridden through an isolated call.
        if arguments.as_object().is_none_or(|o| o.keys().any(|key|
            matches!(key.as_str(), "access_token" | "refresh_token" | "api_key" | "password" | "credentials" | "token"
                | "account" | "account_id" | "credential_id" | "connection_string" | "base_url" | "endpoint"))) {
            return Err(VerificationError::Invalid);
        }
        let policy = product_policy.ok_or(VerificationError::Unavailable)?;
        if !policy.supports(tool) { return Err(VerificationError::Unavailable); }
        let candidates = policy.candidates_for_call(context, tool, provider, arguments)?;
        if candidates.len() > 128 { return Err(VerificationError::Invalid); }
        let mut permitted = Vec::new();
        for candidate in candidates {
            if explicit.is_some_and(|account| account != candidate.account()) { continue; }
            let binding = candidate.binding()?;
            if !employee_scope && !context.record().io_resources().any(|grant|
                matches!(grant, ResourceGrant::Connector { account, binding: allowed }
                    if account.as_str() == candidate.account() && allowed == &binding)) { continue; }
            // The product adapter cannot authorize another person's private
            // credential, even if a requester/consent implementation errs.
            if let EmployeeBinding::Personal(owner) = &binding {
                if !context.record().personal_owner_matches(owner.as_str()) { continue; }
            }
            if let (EmployeeBinding::Shared(employee), EmployeeBinding::Shared(account)) =
                (&context.record().employee_binding, &binding) {
                if employee != account { continue; }
            }
            if catalog.matches(candidate.account(), provider)? { permitted.push((candidate, binding)); }
        }
        if tool == "db_sources" && explicit.is_none() {
            // A collection anchor is chosen only after the same owner, finite
            // grant, catalog AND binding checks as each individual member.
            permitted.sort_by(|(a,_),(b,_)|a.account().cmp(b.account()));
            for (candidate,binding) in permitted {
                let selected=Self { account:candidate.account().to_owned(),provider,binding,tool:tool.to_owned(),
                    arguments:arguments.clone(),product_selection:(policy.clone(),candidate),context:context.clone(),
                    bindings:bindings.clone(),catalog:catalog.clone() };
                if selected.recheck().is_ok() { return Ok(selected); }
            }
            return Err(VerificationError::BindingMismatch);
        }
        if permitted.len() != 1 { return Err(VerificationError::BindingMismatch); }
        let (candidate, binding) = permitted.remove(0);
        let selected = Self { account: candidate.account().to_owned(), provider, binding, tool: tool.to_owned(),
            arguments: arguments.clone(), product_selection: (policy, candidate), context: context.clone(), bindings, catalog };
        selected.recheck()?;
        Ok(selected)
    }
}

/// Boot injects account-specific connector adapters. A missing adapter refuses;
/// an adapter must retain all provider/recipient/egress gates and recheck before
/// sending. It must never forward the call to a legacy home-account selector.
pub trait BoundConnectorExecutor: Send + Sync {
    fn supports(&self, tool: &str) -> bool;
    fn execute<'a>(&'a self, selected: SelectedConnector, call: AuthorizedToolCall)
        -> Pin<Box<dyn Future<Output = Result<Value, VerificationError>> + Send + 'a>>;
}

#[cfg(test)]
#[path = "isolation_connectors_tests.rs"]
mod tests;
