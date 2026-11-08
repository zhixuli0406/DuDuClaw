//! Account-bound Odoo execution retains the original MCP business policy.
use std::{future::Future, pin::Pin, sync::Arc};
use duduclaw_gateway::{isolation_connector_odoo::OdooConnectorProvider,
    isolation_connectors::{BoundConnectorExecutor, SelectedConnector}};
use duduclaw_mcp_boundary::{tool_policy::AuthorizedToolCall, VerificationError};
use serde_json::Value;

/// Exact tools handled by the retained Odoo business dispatcher.
pub const ODOO_EXECUTOR_TOOLS: &[&str] = &[
    "odoo_connect", "odoo_status", "odoo_crm_leads", "odoo_crm_create_lead", "odoo_crm_update_stage",
    "odoo_sale_orders", "odoo_sale_create_quotation", "odoo_sale_confirm", "odoo_inventory_products",
    "odoo_inventory_check", "odoo_invoice_list", "odoo_payment_status", "odoo_search", "odoo_execute",
    "odoo_report", "odoo_partner_search", "odoo_schema_overview", "odoo_schema_fields",
];

pub struct OdooBoundExecutor { provider: Arc<OdooConnectorProvider> }
impl OdooBoundExecutor {
    pub fn new(provider: Arc<OdooConnectorProvider>) -> Self { Self { provider } }
}
impl BoundConnectorExecutor for OdooBoundExecutor {
    fn supports(&self, tool: &str) -> bool { ODOO_EXECUTOR_TOOLS.contains(&tool) }
    fn execute<'a>(&'a self, selected: SelectedConnector, call: AuthorizedToolCall)
        -> Pin<Box<dyn Future<Output = Result<Value, VerificationError>> + Send + 'a>> {
        Box::pin(async move {
            if !self.supports(call.name()) { return Err(VerificationError::Unavailable); }
            selected.verify_call(&call)?;
            // Reject disallowed operations before credential resolution or an
            // authentication handshake. The handler repeats these same gates.
            self.provider.check_operation(crate::mcp::classify_odoo_call(call.name(), call.arguments()))?;
            let prepared = self.provider.prepare(selected, &call).await?;
            let pool = Arc::new(crate::odoo_pool::OdooConnectorPool::new(prepared.global));
            if let Some(config) = prepared.agent {
                pool.register_agent(self.provider.agent(), config).await;
            }
            prepared.connector.verify_io().map_err(|_| VerificationError::Unavailable)?;
            let connector = prepared.connector.clone();
            let result = crate::mcp::handle_bound_odoo_tool(call.name(), call.arguments(), self.provider.home(),
                &pool, self.provider.agent(), prepared.connector).await;
            if connector.verify_io().is_err() {
                return Ok(serde_json::json!({"isError":true,"content":[{"type":"text", "text":
                    "無法確認 Odoo 操作的結果；若有寫入，請先到 Odoo 檢查，勿直接重試。"}]}));
            }
            Ok(result)
        })
    }
}
