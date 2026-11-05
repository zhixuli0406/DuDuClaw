//! Account selection for isolated MCP. No credential fallback or ambient account.
use std::{future::Future, pin::Pin, sync::{Arc, Mutex}};
use duduclaw_isolation::capability::ResourceGrant;
use duduclaw_mcp_boundary::{bindings::BindingStore, tool_policy::AuthorizedToolCall,
    RequestContext, VerificationError};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;

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
            || !matches!(provider, "google" | "notion" | "github" | "odoo" | "database" | "channel" | "search" | "model") {
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
        _ => return None,
    })
}

/// Constructible only through selection and live checks. Executors must use
/// this exact account; never select defaults from home, environment or config.
pub struct SelectedConnector {
    account: String,
    provider: &'static str,
    context: RequestContext,
    bindings: Arc<Mutex<BindingStore>>,
    catalog: Arc<ConnectorCatalog>,
}
impl SelectedConnector {
    pub fn account(&self) -> &str { &self.account }
    pub fn provider(&self) -> &'static str { self.provider }
    pub fn context(&self) -> &RequestContext { &self.context }
    /// Repeat immediately before credential acquisition and every effect/retry.
    pub fn recheck(&self) -> Result<(), VerificationError> {
        let record = self.context.record();
        let now = duduclaw_mcp_boundary::authority::now_seconds()?;
        if now < record.not_before || now >= record.expires_at { return Err(VerificationError::Invalid); }
        self.bindings.lock().map_err(|_| VerificationError::Unavailable)?
            .authorize_connector(record.employee, &record.employee_binding, &record.principal, &self.account)
            .map_err(|_| VerificationError::BindingMismatch)?;
        if !self.catalog.matches(&self.account, self.provider)? { return Err(VerificationError::BindingMismatch); }
        Ok(())
    }
    pub fn select(context: &RequestContext, tool: &str, arguments: &Value,
        bindings: Arc<Mutex<BindingStore>>, catalog: Arc<ConnectorCatalog>) -> Result<Self, VerificationError> {
        let provider = provider_for_tool(tool).ok_or(VerificationError::Invalid)?;
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
        let mut candidates = Vec::new();
        for grant in context.record().resources.as_slice() {
            if let ResourceGrant::Connector { account, binding } = grant {
                if binding == &context.record().employee_binding
                    && explicit.is_none_or(|selected| selected == account.as_str())
                    && catalog.matches(account.as_str(), provider)? {
                    candidates.push(account.as_str());
                }
            }
        }
        if candidates.len() != 1 { return Err(VerificationError::BindingMismatch); }
        let selected = Self { account: candidates[0].to_owned(), provider, context: context.clone(), bindings, catalog };
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
