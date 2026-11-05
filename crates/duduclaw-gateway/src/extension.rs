//! Plugin extension point for the DuDuClaw gateway.
//!
//! The gateway ships with [`NullExtension`] (no-op). Third-party or future
//! plugins can implement [`GatewayExtension`] to inject extra RPC methods
//! and Axum routes without modifying core gateway code.

use async_trait::async_trait;
use serde_json::Value;

use duduclaw_auth::UserContext;

use crate::protocol::WsFrame;

mod http;
pub use http::ExtensionRoute;
pub(crate) use http::merge_routes;

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

    /// Stable lowercase ASCII identifier used by the reserved RPC/HTTP namespaces.
    fn extension_id(&self) -> &str {
        "duduclaw"
    }

    /// Exact names under `ext.<extension_id>.<method>`. Unlisted calls are refused.
    fn rpc_methods(&self) -> Vec<String> {
        Vec::new()
    }

    /// Handle a plugin-exclusive RPC method.
    ///
    /// Called only after every core dispatcher declines the method and only
    /// for names accepted at registration. `None` means unknown method.
    async fn handle_method(
        &self,
        _method: &str,
        _params: Value,
        _ctx: &UserContext,
    ) -> Option<WsFrame> {
        None
    }

    /// Legacy opaque routers cannot be validated and are refused at startup.
    /// Migrate to `http_routes`; kept here so old implementations fail closed.
    fn extra_routes(&self) -> Option<axum::Router> {
        None
    }

    /// Literal paths under `/ext/<extension_id>/`, protected by gateway middleware.
    fn http_routes(&self) -> Vec<ExtensionRoute> {
        Vec::new()
    }

    /// Binary update channel for this deployment.
    ///
    /// `None` (the default) leaves the gateway on its built-in path: the public
    /// GitHub release channel for CE, and — for the closed `duduclaw-pro`
    /// wrapper, which the public asset would overwrite with the open binary —
    /// check-and-notify with no install offered.
    ///
    /// A plugin returns `Some(..)` to supply both the update source and its own
    /// pinned signature key; see [`crate::updater::UpdateProvider`].
    fn update_provider(&self) -> Option<std::sync::Arc<dyn crate::updater::UpdateProvider>> {
        None
    }
}

/// No-op extension (default).
pub struct NullExtension;

#[async_trait]
impl GatewayExtension for NullExtension {}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64
        && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn registered_methods(extension: &dyn GatewayExtension) -> Result<std::collections::BTreeSet<String>, String> {
    let id = extension.extension_id();
    if !valid_id(id) {
        return Err("invalid extension identifier".into());
    }
    let mut registered = std::collections::BTreeSet::new();
    for method in extension.rpc_methods() {
        let mut parts = method.split('.');
        if parts.next() != Some("ext") || parts.next() != Some(id)
            || !parts.clone().all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'))
            || parts.next().is_none()
        {
            return Err(format!("RPC method {method:?} is reserved or outside ext.{id}.<method>"));
        }
        if !registered.insert(method.clone()) {
            return Err(format!("duplicate extension RPC method {method:?}"));
        }
    }
    Ok(registered)
}

struct RegisteredExtension {
    inner: std::sync::Arc<dyn GatewayExtension>,
    methods: std::collections::BTreeSet<String>,
}

/// Freeze the declaration once. A changing or catch-all plugin cannot widen it.
pub(crate) fn register(extension: std::sync::Arc<dyn GatewayExtension>) -> std::sync::Arc<dyn GatewayExtension> {
    let methods = registered_methods(extension.as_ref()).unwrap_or_else(|error| {
        tracing::warn!(extension = extension.name(), %error, "extension RPC registration refused; all extension RPC methods disabled");
        std::collections::BTreeSet::new()
    });
    std::sync::Arc::new(RegisteredExtension { inner: extension, methods })
}

#[async_trait]
impl GatewayExtension for RegisteredExtension {
    fn name(&self) -> &str { self.inner.name() }
    fn extension_id(&self) -> &str { self.inner.extension_id() }
    fn rpc_methods(&self) -> Vec<String> { self.methods.iter().cloned().collect() }
    async fn handle_method(&self, method: &str, params: Value, ctx: &UserContext) -> Option<WsFrame> {
        if !self.methods.contains(method) { return None; }
        self.inner.handle_method(method, params, ctx).await
    }
    fn extra_routes(&self) -> Option<axum::Router> { self.inner.extra_routes() }
    fn http_routes(&self) -> Vec<ExtensionRoute> { self.inner.http_routes() }
    fn update_provider(&self) -> Option<std::sync::Arc<dyn crate::updater::UpdateProvider>> {
        self.inner.update_provider()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};

    struct GreedyExtension {
        methods: Vec<String>,
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl GatewayExtension for GreedyExtension {
        fn extension_id(&self) -> &str { "test" }
        fn rpc_methods(&self) -> Vec<String> { self.methods.clone() }
        async fn handle_method(&self, _: &str, _: Value, _: &UserContext) -> Option<WsFrame> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Some(WsFrame::ok_response("", serde_json::json!({"extension": true})))
        }
    }

    #[tokio::test]
    async fn extension_rpc_core_registration_is_refused() {
        for method in ["users.list", "approvals.decide", "ping", "mfa.status", "ext.other.call"] {
            let calls = Arc::new(AtomicUsize::new(0));
            let extension = Arc::new(GreedyExtension {
                methods: vec![method.into(), "ext.test.call".into()], calls: calls.clone(),
            });
            assert!(registered_methods(extension.as_ref()).is_err(), "{method}");
            let registered = register(extension);
            for name in [method, "ext.test.call"] {
                assert!(registered.handle_method(name, Value::Null, &UserContext::admin_fallback()).await.is_none());
            }
            assert_eq!(calls.load(Ordering::SeqCst), 0);
        }
    }

    #[tokio::test]
    async fn extension_rpc_core_first_and_undeclared_calls_never_reach_plugin() {
        let home = tempfile::tempdir().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let handler = crate::handlers::MethodHandler::with_extension(home.path().to_path_buf(), Arc::new(GreedyExtension {
            methods: vec!["ext.test.call".into()], calls: calls.clone(),
        })).await;
        let ctx = UserContext::admin_fallback();
        for method in ["ping", "users.list", "approvals.decide", "ext.test.undeclared"] {
            let _ = handler.handle(method, serde_json::json!({}), &ctx).await;
            assert_eq!(calls.load(Ordering::SeqCst), 0, "{method}");
        }
        let _ = handler.handle("ext.test.call", Value::Null, &ctx).await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn extension_rpc_rejects_empty_duplicate_and_invalid_names() {
        for methods in [vec!["ext.test."], vec!["ext.test"], vec!["ext.test.a..b"],
            vec!["ext.test.call", "ext.test.call"], vec!["ext.test.call/path"]] {
            let extension = GreedyExtension { methods: methods.into_iter().map(str::to_string).collect(), calls: Default::default() };
            assert!(registered_methods(&extension).is_err());
        }
    }

    #[test]
    fn null_extension_defaults() {
        let ext = NullExtension;
        assert_eq!(ext.name(), "DuDuClaw");
        assert!(ext.extra_routes().is_none());
        // No provider ⇒ the gateway keeps its built-in update behavior.
        assert!(ext.update_provider().is_none());
    }
}
