//! Bound retained-history adapter keeps the original live routing/injection gates.
use std::{future::Future, pin::Pin, sync::Arc};
use duduclaw_gateway::{isolation_connector_channel::{ChannelConnectorProvider,CHANNEL_TOOLS},
    isolation_connectors::{BoundConnectorExecutor,SelectedConnector}};
use duduclaw_mcp_boundary::{tool_policy::AuthorizedToolCall,VerificationError};
use serde_json::Value;

pub struct ChannelBoundExecutor { provider: Arc<ChannelConnectorProvider> }
impl ChannelBoundExecutor { pub fn new(provider: Arc<ChannelConnectorProvider>) -> Self { Self { provider } } }
impl BoundConnectorExecutor for ChannelBoundExecutor {
    fn supports(&self, tool: &str) -> bool { CHANNEL_TOOLS.contains(&tool) }
    fn execute<'a>(&'a self, selected: SelectedConnector, call: AuthorizedToolCall)
        -> Pin<Box<dyn Future<Output=Result<Value,VerificationError>>+Send+'a>> {
        Box::pin(async move {
            self.provider.prepare(&selected,&call)?;
            if call.name() != "group_history_read" { return self.provider.execute(selected,call).await; }
            let selected = Arc::new(selected);
            let result = duduclaw_mcp_boundary::io_authority::scope(selected.io_context(),
                crate::mcp::handle_bound_group_history(call.arguments(),self.provider.home(),self.provider.agent())).await;
            selected.recheck()?;
            Ok(result)
        })
    }
}
