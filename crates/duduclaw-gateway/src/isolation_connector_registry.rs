//! Boot registry for account-bound provider adapters. A registered adapter must
//! implement both live product selection and execution for its declared tools.
use std::{collections::BTreeMap, future::Future, pin::Pin, sync::Arc};
use duduclaw_mcp_boundary::{tool_policy::{AuthorizedToolCall, POLICIES, Route}, RequestContext, VerificationError};
use serde_json::Value;
use crate::isolation_connectors::{BoundConnectorExecutor, LiveConnectorSelectionPolicy,
    ProductConnectorAccount, SelectedConnector, provider_for_tool};

struct Adapter {
    policy: Arc<dyn LiveConnectorSelectionPolicy>,
    executor: Arc<dyn BoundConnectorExecutor>,
}

#[derive(Default)]
pub struct ConnectorProviderRegistry { adapters: BTreeMap<&'static str, Adapter> }
impl ConnectorProviderRegistry {
    /// Registration is explicit and immutable after boot. Overlap, unknown
    /// tools and a policy/executor disagreement all refuse the registration.
    pub fn register(&mut self, tools: &[&'static str], policy: Arc<dyn LiveConnectorSelectionPolicy>,
        executor: Arc<dyn BoundConnectorExecutor>) -> Result<(), VerificationError> {
        let mut seen = std::collections::BTreeSet::new();
        if tools.is_empty() || tools.iter().any(|tool| !seen.insert(*tool)
            || self.adapters.contains_key(tool) || provider_for_tool(tool).is_none()
            || !policy.supports(tool) || !executor.supports(tool)) {
            return Err(VerificationError::Invalid);
        }
        for tool in tools { self.adapters.insert(tool, Adapter { policy: policy.clone(), executor: executor.clone() }); }
        Ok(())
    }
    /// Register a whole provider atomically. Partial policy/executor support
    /// refuses before changing the registry; the table is the sole inventory.
    pub fn register_provider(&mut self, provider: &str,
        policy: Arc<dyn LiveConnectorSelectionPolicy>, executor: Arc<dyn BoundConnectorExecutor>)
        -> Result<(), VerificationError> {
        let tools: Vec<_> = POLICIES.iter().filter(|p| p.route == Route::Connector
            && provider_for_tool(p.name) == Some(provider)).map(|p| p.name).collect();
        self.register(&tools, policy, executor)
    }
    /// Full parity is an install/startup condition, never an optimistic label.
    pub fn require_complete(&self) -> Result<(), VerificationError> {
        if POLICIES.iter().filter(|p| p.route == Route::Connector)
            .any(|p| !self.ready(p.name)) { return Err(VerificationError::Unavailable); }
        Ok(())
    }
    pub fn missing_tools(&self) -> Vec<&'static str> {
        POLICIES.iter().filter(|p| p.route == Route::Connector && !self.ready(p.name))
            .map(|p| p.name).collect()
    }
    fn ready(&self, tool: &str) -> bool {
        self.adapters.get(tool).is_some_and(|a| a.policy.supports(tool) && a.executor.supports(tool))
    }
}
impl LiveConnectorSelectionPolicy for ConnectorProviderRegistry {
    fn supports(&self, tool: &str) -> bool { self.ready(tool) }
    fn candidates(&self, context: &RequestContext, tool: &str, provider: &str)
        -> Result<Vec<ProductConnectorAccount>, VerificationError> {
        self.candidates_for_call(context, tool, provider, &Value::Object(Default::default()))
    }
    fn candidates_for_call(&self, context: &RequestContext, tool: &str, provider: &str, arguments: &Value)
        -> Result<Vec<ProductConnectorAccount>, VerificationError> {
        context.verify_live(duduclaw_mcp_boundary::authority::now_seconds()?)?;
        if provider_for_tool(tool) != Some(provider) || !self.ready(tool) { return Err(VerificationError::Unavailable); }
        self.adapters.get(tool).ok_or(VerificationError::Unavailable)?.policy.candidates_for_call(context, tool, provider, arguments)
    }
}
impl BoundConnectorExecutor for ConnectorProviderRegistry {
    fn supports(&self, tool: &str) -> bool { self.ready(tool) }
    fn execute<'a>(&'a self, selected: SelectedConnector, call: AuthorizedToolCall)
        -> Pin<Box<dyn Future<Output = Result<Value, VerificationError>> + Send + 'a>> {
        Box::pin(async move {
            selected.verify_call(&call)?;
            if !self.ready(call.name()) { return Err(VerificationError::Unavailable); }
            let executor = &self.adapters.get(call.name()).ok_or(VerificationError::Unavailable)?.executor;
            executor.execute(selected, call).await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct AdapterStub;
    impl LiveConnectorSelectionPolicy for AdapterStub {
        fn supports(&self, tool: &str) -> bool { tool == "db_sources" }
        fn candidates(&self, _: &RequestContext, _: &str, _: &str) -> Result<Vec<ProductConnectorAccount>, VerificationError> {
            Ok(vec![])
        }
    }
    impl BoundConnectorExecutor for AdapterStub {
        fn supports(&self, tool: &str) -> bool { tool == "db_sources" }
        fn execute<'a>(&'a self, _: SelectedConnector, _: AuthorizedToolCall)
            -> Pin<Box<dyn Future<Output = Result<Value, VerificationError>> + Send + 'a>> {
            Box::pin(async { Err(VerificationError::Unavailable) })
        }
    }
    #[test]
    fn registry_refuses_missing_duplicate_unknown_or_unsupported_providers() {
        let mut registry = ConnectorProviderRegistry::default();
        assert!(registry.require_complete().is_err());
        let policy = Arc::new(AdapterStub);
        let executor = Arc::new(AdapterStub);
        for tools in [vec![], vec!["unknown"], vec!["db_query"], vec!["db_sources", "db_sources"]] {
            assert!(registry.register(&tools, policy.clone(), executor.clone()).is_err());
        }
        assert!(!registry.ready("db_sources"));
        registry.register(&["db_sources"], policy.clone(), executor.clone()).unwrap();
        assert!(registry.ready("db_sources"));
        assert!(registry.register(&["db_sources"], policy, executor).is_err());
        assert!(!registry.missing_tools().contains(&"db_sources"));
        assert!(registry.missing_tools().contains(&"db_query"));
        assert!(registry.require_complete().is_err());
    }
    #[test]
    fn whole_provider_registration_is_atomic_and_policy_inventory_is_complete() {
        let mut registry = ConnectorProviderRegistry::default();
        assert!(registry.register_provider("database", Arc::new(AdapterStub), Arc::new(AdapterStub)).is_err());
        assert_eq!(registry.missing_tools().len(), POLICIES.iter().filter(|p| p.route == Route::Connector).count());
        assert!(!registry.ready("db_sources"), "partial registration must not leak into readiness");
        assert!(registry.register_provider("unrecognized", Arc::new(AdapterStub), Arc::new(AdapterStub)).is_err());
        struct Complete;
        impl LiveConnectorSelectionPolicy for Complete {
            fn supports(&self, tool: &str) -> bool { provider_for_tool(tool).is_some() }
            fn candidates(&self, _: &RequestContext, _: &str, _: &str) -> Result<Vec<ProductConnectorAccount>, VerificationError> { Ok(vec![]) }
        }
        impl BoundConnectorExecutor for Complete {
            fn supports(&self, tool: &str) -> bool { provider_for_tool(tool).is_some() }
            fn execute<'a>(&'a self, _: SelectedConnector, _: AuthorizedToolCall)
                -> Pin<Box<dyn Future<Output = Result<Value, VerificationError>> + Send + 'a>> {
                Box::pin(async { Err(VerificationError::Unavailable) })
            }
        }
        for provider in ["google", "notion", "github", "odoo", "database", "channel", "search", "model", "computer"] {
            registry.register_provider(provider, Arc::new(Complete), Arc::new(Complete)).unwrap();
        }
        assert!(registry.missing_tools().is_empty());
        assert!(registry.require_complete().is_ok());
        assert!(registry.register_provider("google", Arc::new(Complete), Arc::new(Complete)).is_err());
    }

}
