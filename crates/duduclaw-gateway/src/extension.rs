//! Extension point for Pro/Enterprise features.
//!
//! CE ships with [`NullExtension`]. The Pro binary replaces this with
//! `ProExtension` that injects extra RPC methods, dashboard routes,
//! evolution parameters, and license information.

use async_trait::async_trait;
use serde_json::Value;

use duduclaw_auth::UserContext;

use crate::protocol::WsFrame;

/// Extension trait for Pro/Enterprise features injected at gateway startup.
///
/// CE uses [`NullExtension`] (no-op). Pro binary provides its own
/// implementation that adds extra RPC methods and overrides.
#[async_trait]
pub trait GatewayExtension: Send + Sync + 'static {
    /// Display name of the extension (e.g. "Pro", "Enterprise").
    fn name(&self) -> &str {
        "Community"
    }

    /// License tier string (used for display in dashboard / system.status).
    fn tier(&self) -> &str {
        "community"
    }

    /// Handle a Pro-exclusive RPC method.
    ///
    /// Returns `None` if the method is not recognized, causing the gateway
    /// to fall through to the CE dispatch table.
    async fn handle_method(
        &self,
        _method: &str,
        _params: Value,
        _ctx: &UserContext,
    ) -> Option<WsFrame> {
        None
    }

    /// Additional Axum routes to merge into the gateway router.
    fn extra_routes(&self) -> Option<axum::Router> {
        None
    }

    /// Override GVU max depth for an agent.
    ///
    /// Returns `None` to use the CE default (3 rounds).
    fn gvu_max_depth(&self, _agent_name: &str) -> Option<u32> {
        None
    }

    /// Override GVU parameters for an agent.
    ///
    /// Returns `None` to use the CE defaults.
    fn gvu_params(&self, _agent_name: &str) -> Option<Value> {
        None
    }

    /// License status info (shown in dashboard + system.status + license.status).
    fn license_info(&self) -> Value {
        serde_json::json!({
            "tier": "community",
            "edition": "Community Edition (Apache-2.0)",
            "activated": false,
        })
    }
}

/// No-op extension for Community Edition builds.
pub struct NullExtension;

#[async_trait]
impl GatewayExtension for NullExtension {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn null_extension_defaults() {
        let ext = NullExtension;
        assert_eq!(ext.name(), "Community");
        assert_eq!(ext.tier(), "community");
        assert!(ext.gvu_max_depth("any").is_none());
        assert!(ext.gvu_params("any").is_none());
        assert!(ext.extra_routes().is_none());

        let info = ext.license_info();
        assert_eq!(info["tier"], "community");
        assert_eq!(info["activated"], false);
    }
}
