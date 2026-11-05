//! Declarative extension routes. Opaque routers cannot bypass this boundary.
use std::sync::Arc;

use axum::{body::{Body, to_bytes}, extract::{Request, State}, http::{HeaderMap, StatusCode},
    middleware::{self, Next}, response::{IntoResponse, Response}, routing::MethodRouter, Router};
use duduclaw_auth::{UserContext, UserRole};

use super::GatewayExtension;

const MAX_BODY_BYTES: usize = 2 * 1024 * 1024;

pub struct ExtensionRoute {
    pub path: String,
    pub methods: MethodRouter,
    /// Required for every route; the constructor defaults to Admin.
    pub minimum_role: UserRole,
}

impl ExtensionRoute {
    /// Every extension route requires authentication and defaults to Admin.
    pub fn new(path: impl Into<String>, methods: MethodRouter) -> Self {
        Self { path: path.into(), methods, minimum_role: UserRole::Admin }
    }
}

pub(crate) type Authenticate = Arc<dyn Fn(&HeaderMap, UserRole) -> Result<UserContext, Response> + Send + Sync>;

#[derive(Clone)]
struct RouteGate {
    minimum_role: UserRole,
    authenticate: Authenticate,
}

fn validate_path(id: &str, path: &str) -> Result<(), String> {
    let mut segments = path.split('/');
    if segments.next() != Some("") || segments.next() != Some("ext")
        || segments.next() != Some(id) || segments.clone().next().is_none()
        || !segments.all(|s| !s.is_empty() && s != "." && s != ".."
            && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.')))
    {
        return Err(format!("extension route {path:?} must be a literal path under /ext/{id}/"));
    }
    Ok(())
}

/// `route_service` owns the whole path, so even different HTTP verbs cannot
/// share a core path. Axum reports conflicts by panic; turn that into a startup
/// error, retaining the core router untouched on refusal.
pub(crate) fn merge_routes(
    mut core: Router,
    extension: &dyn GatewayExtension,
    authenticate: Authenticate,
) -> Result<Router, String> {
    if extension.extra_routes().is_some() {
        return Err("opaque extension extra_routes refused; migrate to declared http_routes under /ext/<extension-id>/".into());
    }
    let routes = extension.http_routes();
    if !super::valid_id(extension.extension_id()) {
        return Err("invalid extension identifier".into());
    }
    let mut paths = std::collections::BTreeSet::new();
    for route in &routes {
        validate_path(extension.extension_id(), &route.path)?;
        if !paths.insert(route.path.clone()) {
            return Err(format!("duplicate extension route {:?}", route.path));
        }
    }
    for route in routes {
        let gate = RouteGate { minimum_role: route.minimum_role, authenticate: authenticate.clone() };
        // The gateway layer is outside all extension-supplied layers.
        let service = route.methods.layer(middleware::from_fn_with_state(gate, route_gate));
        core = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            core.route_service(&route.path, service)
        })).map_err(|_| format!("extension route {:?} conflicts with an existing route", route.path))?;
    }
    Ok(core)
}

async fn route_gate(State(gate): State<RouteGate>, mut request: Request, next: Next) -> Response {
    // A malformed header is not the same as an absent Origin.
    if request.headers().get_all("origin").iter().count() > 1
        || request.headers().get("origin").is_some_and(|v| v.to_str().is_err())
        || !crate::server::origin_is_allowed(request.headers())
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    match (gate.authenticate)(request.headers(), gate.minimum_role) {
        Ok(ctx) => { request.extensions_mut().insert(ctx); }
        Err(response) => return response,
    }
    let (parts, body) = request.into_parts();
    // Enforce the cap even for raw Body extractors and extensions that disable
    // DefaultBodyLimit; chunked bodies receive exactly the same bound.
    let bytes = match tokio::time::timeout(std::time::Duration::from_secs(30), to_bytes(body, MAX_BODY_BYTES)).await {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(_)) => return StatusCode::PAYLOAD_TOO_LARGE.into_response(),
        Err(_) => return StatusCode::REQUEST_TIMEOUT.into_response(),
    };
    next.run(Request::from_parts(parts, Body::from(bytes))).await
}

#[cfg(test)]
mod tests;
