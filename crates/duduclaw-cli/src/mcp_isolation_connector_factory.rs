//! Host-owned executor factories. Each invocation retains only the gateway's
//! employee-bound provider; no account discovery or home-account fallback.
use std::sync::Arc;
use duduclaw_gateway::{
    isolation_connector_boot::{ChannelExecutorFactory, OdooExecutorFactory},
    isolation_connector_channel::{ChannelConnectorProvider, CHANNEL_TOOLS},
    isolation_connector_odoo::OdooConnectorProvider,
    isolation_connectors::BoundConnectorExecutor,
};
use duduclaw_mcp_boundary::VerificationError;
use crate::{
    mcp_isolation_connector_channel::ChannelBoundExecutor,
    mcp_isolation_connector_odoo::{OdooBoundExecutor, ODOO_EXECUTOR_TOOLS},
};

pub struct CliOdooExecutorFactory;
impl OdooExecutorFactory for CliOdooExecutorFactory {
    fn supports(&self, tool: &str) -> bool { ODOO_EXECUTOR_TOOLS.contains(&tool) }
    fn executor(&self, provider: Arc<OdooConnectorProvider>) -> Result<Arc<dyn BoundConnectorExecutor>, VerificationError> {
        Ok(Arc::new(OdooBoundExecutor::new(provider)))
    }
}

pub struct CliChannelExecutorFactory;
impl ChannelExecutorFactory for CliChannelExecutorFactory {
    fn supports(&self, tool: &str) -> bool { CHANNEL_TOOLS.contains(&tool) }
    fn executor(&self, provider: Arc<ChannelConnectorProvider>) -> Result<Arc<dyn BoundConnectorExecutor>, VerificationError> {
        Ok(Arc::new(ChannelBoundExecutor::new(provider)))
    }
}

#[cfg(target_os = "linux")]
pub(crate) fn install() -> Result<(), VerificationError> {
    duduclaw_gateway::isolation_connector_boot::install_connector_executor_factories(
        duduclaw_gateway::isolation_connector_boot::ConnectorExecutorFactories {
            odoo: Arc::new(CliOdooExecutorFactory), channel: Arc::new(CliChannelExecutorFactory),
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use duduclaw_gateway::isolation_connector_channel::ChannelAccount;
    use duduclaw_gateway::isolation_connectors::{ProductConnectorAccount, provider_for_tool};
    use duduclaw_isolation::ids::EmployeeId;

    fn fixture() -> tempfile::TempDir {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("config.toml"),
            "[odoo]\nurl='https://odoo.example.com'\ndb='demo'\nusername='shared'\napi_key_enc='shared-reference'\n").unwrap();
        for name in ["alice", "bob"] {
            let dir = home.path().join("agents").join(name);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("agent.toml"), format!(
                "[odoo]\nusername='{name}'\napi_key_enc='{name}-reference'\n")).unwrap();
        }
        home
    }
    fn account(owner: &str, provider: &str) -> ProductConnectorAccount {
        ProductConnectorAccount::Personal { account: format!("{owner}-{provider}"), owner: owner.into() }
    }
    fn odoo(home: &std::path::Path, owner: &str, employee: EmployeeId) -> Arc<OdooConnectorProvider> {
        Arc::new(OdooConnectorProvider::new(home.into(), owner.into(), employee, account(owner, "odoo")).unwrap())
    }
    fn channel(home: &std::path::Path, owner: &str, employee: EmployeeId) -> Arc<ChannelConnectorProvider> {
        let route = ChannelAccount::new(account(owner, "channel"), "telegram".into(),
            format!("{owner}-chat"), "telegram".into(), Some(format!("{owner}-token")), None).unwrap();
        Arc::new(ChannelConnectorProvider::new(home.into(), owner.into(), employee, vec![route]).unwrap())
    }

    #[test]
    fn factory_creates_executor_bound_to_one_account() {
        let home = fixture();
        let alice = EmployeeId([1; 16]);
        let bob = EmployeeId([2; 16]);
        let odoo_a = odoo(home.path(), "alice", alice);
        let odoo_b = odoo(home.path(), "bob", bob);
        let channel_a = channel(home.path(), "alice", alice);
        let channel_b = channel(home.path(), "bob", bob);
        // Each provider above contains just that employee's account. The factory
        // must retain the exact provider passed on every invocation, including
        // when another employee has already used the same stateless factory.
        let a = CliOdooExecutorFactory.executor(odoo_a.clone()).unwrap();
        let b = CliOdooExecutorFactory.executor(odoo_b.clone()).unwrap();
        let ca = CliChannelExecutorFactory.executor(channel_a.clone()).unwrap();
        let cb = CliChannelExecutorFactory.executor(channel_b.clone()).unwrap();
        for count in [Arc::strong_count(&odoo_a), Arc::strong_count(&odoo_b),
            Arc::strong_count(&channel_a), Arc::strong_count(&channel_b)] { assert_eq!(count, 2); }
        drop((a, ca));
        assert_eq!(Arc::strong_count(&odoo_a), 1);
        assert_eq!(Arc::strong_count(&channel_a), 1);
        assert_eq!(Arc::strong_count(&odoo_b), 2);
        assert_eq!(Arc::strong_count(&channel_b), 2);
        drop((b, cb));
        assert_eq!(Arc::strong_count(&odoo_b), 1);
        assert_eq!(Arc::strong_count(&channel_b), 1);
    }

    #[test]
    fn factory_declared_coverage_matches_executor_tools() {
        let home = fixture();
        let employee = EmployeeId([1; 16]);
        let odoo = CliOdooExecutorFactory.executor(odoo(home.path(), "alice", employee)).unwrap();
        let channel = CliChannelExecutorFactory.executor(channel(home.path(), "alice", employee)).unwrap();
        let mut tools: BTreeSet<&str> = duduclaw_mcp_boundary::tool_policy::POLICIES.iter().map(|p| p.name).collect();
        tools.extend(crate::mcp::tools().map(|tool| tool.name));
        tools.extend(ODOO_EXECUTOR_TOOLS.iter().copied());
        tools.extend(CHANNEL_TOOLS.iter().copied());
        tools.extend(["odoo_unknown", "channel_unknown", "odoo_status_suffix", "send_message_suffix", ""]);
        for tool in tools {
            assert_eq!(CliOdooExecutorFactory.supports(tool), odoo.supports(tool), "Odoo: {tool}");
            assert_eq!(CliChannelExecutorFactory.supports(tool), channel.supports(tool), "channel: {tool}");
            assert_eq!(odoo.supports(tool), provider_for_tool(tool) == Some("odoo"), "Odoo policy: {tool}");
            assert_eq!(channel.supports(tool), provider_for_tool(tool) == Some("channel"), "channel policy: {tool}");
        }
    }
}
