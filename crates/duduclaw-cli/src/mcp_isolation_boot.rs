//! Binary-owned original MCP dispatcher adapter for protected gateway boot.
//! Authentication stays inside the gateway; no key enters runtime config/fds.
use std::{path::PathBuf, sync::Arc};
use duduclaw_isolation::ids::EmployeeId;
use duduclaw_mcp_boundary::resources::ResourceRoots;
use duduclaw_gateway::{isolation_connector_boot::InstalledConnectorProviders,
    isolation_execution::GatewayFactories};
use crate::{mcp_auth::Principal, mcp_isolation::IsolationDispatcher};
#[cfg(target_os = "linux")]
use duduclaw_gateway::isolation_connectors::{BoundConnectorExecutor, LiveConnectorSelectionPolicy};
#[cfg(target_os = "linux")]
use duduclaw_gateway::isolation_mcp::{DispatcherFactory, DispatcherInput, Dispatcher};
use duduclaw_gateway::isolation_mcp::VerificationError;

#[cfg(target_os = "linux")]
pub(crate) struct CliDispatcherFactory;
#[cfg(target_os = "linux")]
impl DispatcherFactory for CliDispatcherFactory {
    fn verify_producers(&self, execution: &dyn duduclaw_mcp_boundary::execution::ExecutionPlanFactory,
        worker: &dyn duduclaw_mcp_boundary::worker_execution::WorkerPlanFactory,
        connectors: &dyn BoundConnectorExecutor, selection: &dyn LiveConnectorSelectionPolicy) -> Result<(), VerificationError> {
        use crate::mcp_isolation::BoundaryRoute;
        for tool in crate::mcp::tools() {
            if crate::mcp_isolation_disposition::intentional_refusal(tool.name).is_some() { continue; }
            let ready = match crate::mcp_isolation::boundary_route(tool.name) {
                Some(BoundaryRoute::File) => true,
                Some(BoundaryRoute::GatewayHandler(_)) => true,
                Some(BoundaryRoute::Resource) => {
                    if duduclaw_gateway::isolation_connectors::provider_for_tool(tool.name).is_some() {
                        connectors.supports(tool.name) && selection.supports(tool.name)
                    } else {
                        crate::mcp_isolation::legacy_resource_handler(tool.name)
                    }
                },
                Some(BoundaryRoute::Execution) => execution.supports(tool.name),
                Some(BoundaryRoute::Worker) => worker.supports(tool.name),
                // Unknown/new routes require an explicit product adapter.
                _ => false,
            };
            if !ready { return Err(VerificationError::Unavailable); }
        }
        Ok(())
    }

    fn ingress_tools(&self, home: &std::path::Path, agent: &str, lane: duduclaw_isolation::capability::Lane)
        -> Result<duduclaw_isolation::bounded::List<duduclaw_isolation::bounded::Text<128>, 256>, VerificationError> {
        if !duduclaw_core::is_valid_agent_id(agent) || duduclaw_core::is_reserved_agent_id(agent) {
            return Err(VerificationError::Invalid);
        }
        let raw = std::fs::read_to_string(home.join("agents").join(agent).join("agent.toml"))
            .map_err(|_| VerificationError::Unavailable)?;
        let config: duduclaw_core::types::AgentConfig = toml::from_str(&raw)
            .map_err(|_| VerificationError::Unavailable)?;
        let caps = &config.capabilities;
        let lane = match lane { duduclaw_isolation::capability::Lane::Standard => duduclaw_core::ProcessLane::Normal,
            duduclaw_isolation::capability::Lane::Explore => duduclaw_core::ProcessLane::Explore };
        let tools = crate::mcp::tools().filter(|tool| {
            crate::mcp_isolation_disposition::intentional_refusal(tool.name).is_none()
                && duduclaw_core::tool_catalog::tool_list_verdict(tool.name, &caps.denied_tools, &caps.allowed_tools)
                    == duduclaw_core::tool_catalog::ToolListVerdict::Allowed
                && lane.permits(duduclaw_core::effect_of(tool.name))
        }).map(|tool| duduclaw_isolation::bounded::Text::new(tool.name.to_owned())
            .map_err(|_| VerificationError::Invalid)).collect::<Result<Vec<_>, _>>()?;
        // Never truncate to fit the capability bound or alter service_manifest.
        duduclaw_isolation::bounded::List::new(tools).map_err(|_| VerificationError::Invalid)
    }

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

#[cfg(target_os = "linux")]
pub(crate) fn install() -> Result<(), VerificationError> {
    duduclaw_gateway::isolation_mcp::install_dispatcher_factory(Arc::new(CliDispatcherFactory))
}

impl IsolationDispatcher {
    /// This is the complete registration entry point for the integration host.
    /// A successful value is implementation readiness, not provider acceptance.
    pub fn boot(home: PathBuf, employee: EmployeeId, agent: String, principal: Principal,
        roots: Arc<ResourceRoots>, connectors: &InstalledConnectorProviders, factories: GatewayFactories)
        -> Result<Self, VerificationError> {
        connectors.verify_identity(&home, &agent, employee)?;
        let dispatcher = Self::new(home, employee, agent, principal, roots)?
            .with_gateway_services(connectors.resolver(), factories, Some(connectors.registry()));
        dispatcher.require_complete_services()?;
        Ok(dispatcher)
    }
}
