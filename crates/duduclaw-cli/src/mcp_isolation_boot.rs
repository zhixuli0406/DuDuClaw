//! Binary-owned final assembly before admitting a protected employee run.
//! The gateway installs concrete provider owners first; no environment/token is
//! copied into the run and no missing A/E service is replaced with local spawn.
use std::{path::PathBuf, sync::Arc};
use duduclaw_isolation::ids::EmployeeId;
use duduclaw_mcp_boundary::{resources::ResourceRoots, VerificationError};
use duduclaw_gateway::{isolation_connector_boot::InstalledConnectorProviders,
    isolation_execution::GatewayFactories};
use crate::{mcp_auth::Principal, mcp_isolation::IsolationDispatcher};

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
