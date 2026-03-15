use crate::protocol::WsFrame;
use serde_json::{json, Value};
use tracing::info;

/// Dispatches incoming RPC methods to the appropriate handler.
pub struct MethodHandler;

impl MethodHandler {
    pub fn new() -> Self {
        Self
    }

    /// Route `method` to the correct handler and return a [`WsFrame`] response.
    ///
    /// The `id` field is intentionally left empty (`""`) in the returned frame;
    /// the caller is responsible for patching it to match the original request.
    pub async fn handle(&self, method: &str, params: Value) -> WsFrame {
        match method {
            "connect.challenge" => self.handle_connect_challenge(params),
            "connect" => self.handle_connect(params),
            "hello-ok" => self.handle_hello_ok(params),
            "tools.catalog" => self.handle_tools_catalog(params),
            "agents.list" => self.handle_agents_list(params).await,
            "agents.status" => self.handle_agents_status(params).await,
            "agents.create" => self.handle_agents_create(params).await,
            "agents.delegate" => self.handle_agents_delegate(params).await,
            "evolution.status" => self.handle_evolution_status(params).await,
            "evolution.skills" => self.handle_evolution_skills(params).await,
            unknown => {
                WsFrame::error_response("", &format!("Unknown method: {}", unknown))
            }
        }
    }

    // ----- Phase 1: OpenClaw core handshake ----------------------------------

    /// Return a random challenge string for Ed25519 auth (placeholder).
    fn handle_connect_challenge(&self, _params: Value) -> WsFrame {
        info!("connect.challenge requested");
        let challenge = uuid::Uuid::new_v4().to_string();
        WsFrame::ok_response("", json!({ "challenge": challenge }))
    }

    /// Accept a connection (token-based for now).
    fn handle_connect(&self, params: Value) -> WsFrame {
        info!("connect requested");
        let version = params
            .get("version")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        WsFrame::ok_response(
            "",
            json!({
                "version": "0.1.0",
                "client_version": version,
                "status": "connected",
            }),
        )
    }

    /// Acknowledge a successful hello handshake.
    fn handle_hello_ok(&self, _params: Value) -> WsFrame {
        info!("hello-ok acknowledged");
        WsFrame::ok_response("", json!({ "ack": true }))
    }

    /// Return the tool catalog exposed by this gateway.
    fn handle_tools_catalog(&self, _params: Value) -> WsFrame {
        info!("tools.catalog requested");
        WsFrame::ok_response(
            "",
            json!({
                "tools": [
                    {
                        "name": "agents.list",
                        "description": "List all registered agents",
                    },
                    {
                        "name": "agents.status",
                        "description": "Get the status of a specific agent",
                    },
                    {
                        "name": "agents.create",
                        "description": "Create a new agent",
                    },
                    {
                        "name": "agents.delegate",
                        "description": "Delegate a task to an agent",
                    },
                    {
                        "name": "evolution.status",
                        "description": "Get the evolution status",
                    },
                    {
                        "name": "evolution.skills",
                        "description": "List available evolution skills",
                    },
                ],
            }),
        )
    }

    // ----- DuDuClaw extension methods (placeholder) --------------------------

    /// List all registered agents.
    async fn handle_agents_list(&self, _params: Value) -> WsFrame {
        info!("agents.list requested");
        WsFrame::ok_response("", json!({ "agents": [] }))
    }

    /// Return status for a single agent.
    async fn handle_agents_status(&self, params: Value) -> WsFrame {
        let agent_id = params
            .get("agent_id")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        info!(agent_id, "agents.status requested");
        WsFrame::ok_response(
            "",
            json!({
                "agent_id": agent_id,
                "status": "unknown",
                "message": "Agent registry not yet implemented",
            }),
        )
    }

    /// Create a new agent (placeholder).
    async fn handle_agents_create(&self, params: Value) -> WsFrame {
        let name = params
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("unnamed");
        info!(name, "agents.create requested");
        WsFrame::ok_response(
            "",
            json!({
                "created": false,
                "message": "Agent creation not yet implemented",
                "requested_name": name,
            }),
        )
    }

    /// Delegate a task to an agent (placeholder).
    async fn handle_agents_delegate(&self, params: Value) -> WsFrame {
        let agent_id = params
            .get("agent_id")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        info!(agent_id, "agents.delegate requested");
        WsFrame::ok_response(
            "",
            json!({
                "delegated": false,
                "message": "Task delegation not yet implemented",
                "agent_id": agent_id,
            }),
        )
    }

    /// Return the current evolution status (placeholder).
    async fn handle_evolution_status(&self, _params: Value) -> WsFrame {
        info!("evolution.status requested");
        WsFrame::ok_response(
            "",
            json!({
                "enabled": false,
                "message": "Evolution subsystem not yet implemented",
            }),
        )
    }

    /// List available evolution skills (placeholder).
    async fn handle_evolution_skills(&self, _params: Value) -> WsFrame {
        info!("evolution.skills requested");
        WsFrame::ok_response("", json!({ "skills": [] }))
    }
}

impl Default for MethodHandler {
    fn default() -> Self {
        Self::new()
    }
}
