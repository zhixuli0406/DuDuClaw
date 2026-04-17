//! Plugin extension point for the DuDuClaw gateway.
//!
//! The gateway ships with [`NullExtension`] (no-op). Third-party or future
//! plugins can implement [`GatewayExtension`] to inject extra RPC methods
//! and Axum routes without modifying core gateway code.

use async_trait::async_trait;
use serde_json::Value;

use duduclaw_auth::UserContext;

use crate::protocol::WsFrame;

/// Extension trait for injecting additional functionality into the gateway.
///
/// The default implementation ([`NullExtension`]) is a no-op. Plugins can
/// provide their own implementation to add custom RPC methods and routes.
#[async_trait]
pub trait GatewayExtension: Send + Sync + 'static {
    /// Display name of the extension (e.g. "MyPlugin").
    fn name(&self) -> &str {
        "DuDuClaw"
    }

    /// Handle a plugin-exclusive RPC method.
    ///
    /// Returns `None` if the method is not recognized, causing the gateway
    /// to fall through to the standard dispatch table.
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
}

/// No-op extension (default).
pub struct NullExtension;

#[async_trait]
impl GatewayExtension for NullExtension {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn null_extension_defaults() {
        let ext = NullExtension;
        assert_eq!(ext.name(), "DuDuClaw");
        assert!(ext.extra_routes().is_none());
    }
}
