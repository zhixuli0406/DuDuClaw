//! Binary-owned original MCP dispatcher adapter for protected gateway boot.
//! Authentication stays inside the gateway; no key enters runtime config/fds.
use std::sync::Arc;
use duduclaw_gateway::isolation_mcp::{DispatcherFactory, DispatcherInput, Dispatcher, VerificationError};

struct CliDispatcherFactory;
impl DispatcherFactory for CliDispatcherFactory {
    fn create(&self, input: DispatcherInput) -> Result<Arc<dyn Dispatcher>, VerificationError> {
        // Reuse the existing authenticator and its expiry/scope checks. Never
        // synthesize an Admin principal from an employee id or environment.
        let principal = duduclaw_gateway::mcp_internal_key::valid_internal_keys(&input.home)
            .into_iter().find_map(|raw| {
                let raw = zeroize::Zeroizing::new(raw);
                crate::mcp_auth::authenticate_with_key(raw.as_str(), &input.home).ok()
                    .filter(|principal| !principal.is_external
                        && principal.client_id == duduclaw_gateway::mcp_internal_key::INTERNAL_CLIENT_ID)
            }).ok_or(VerificationError::Unavailable)?;
        let dispatcher = crate::mcp_isolation::IsolationDispatcher::new(input.home, input.employee,
            input.agent, principal, input.roots)?
            .with_gateway_services(input.resolver, input.factories, input.connectors)
            .with_turn_resolver(input.turns);
        dispatcher.require_complete_services()?;
        Ok(Arc::new(dispatcher))
    }
}

pub(crate) fn install() -> Result<(), VerificationError> {
    duduclaw_gateway::isolation_mcp::install_dispatcher_factory(Arc::new(CliDispatcherFactory))
}
