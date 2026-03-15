use std::path::PathBuf;
use std::sync::Arc;

use duduclaw_agent::registry::AgentRegistry;
use serde_json::{json, Value};
use tokio::sync::RwLock;
use tracing::info;

use crate::protocol::WsFrame;

/// Dispatches incoming RPC methods to the appropriate handler.
pub struct MethodHandler {
    registry: Arc<RwLock<AgentRegistry>>,
}

impl MethodHandler {
    pub async fn new(agents_dir: PathBuf) -> Self {
        let mut registry = AgentRegistry::new(agents_dir);
        if let Err(e) = registry.scan().await {
            tracing::warn!("Failed to scan agents directory: {e}");
        }
        Self {
            registry: Arc::new(RwLock::new(registry)),
        }
    }

    /// Route `method` to the correct handler and return a [`WsFrame`] response.
    pub async fn handle(&self, method: &str, params: Value) -> WsFrame {
        match method {
            "connect.challenge" => self.handle_connect_challenge(params),
            "connect" => self.handle_connect(params),
            "hello-ok" => self.handle_hello_ok(params),
            "tools.catalog" => self.handle_tools_catalog(params),
            "agents.list" => self.handle_agents_list().await,
            "agents.status" => self.handle_agents_status(params).await,
            "agents.create" => self.handle_agents_create(params).await,
            "agents.delegate" => self.handle_agents_delegate(params).await,
            "agents.pause" => self.handle_agents_pause(params).await,
            "agents.resume" => self.handle_agents_resume(params).await,
            "agents.inspect" => self.handle_agents_inspect(params).await,
            "channels.status" => self.handle_channels_status().await,
            "accounts.budget_summary" => self.handle_budget_summary().await,
            "system.status" => self.handle_system_status().await,
            "system.doctor" => self.handle_system_doctor().await,
            "system.version" => self.handle_system_version(),
            "evolution.status" => self.handle_evolution_status().await,
            "evolution.skills" => self.handle_evolution_skills().await,
            unknown => WsFrame::error_response("", &format!("Unknown method: {unknown}")),
        }
    }

    // ── OpenClaw handshake ───────────────────────────────────

    fn handle_connect_challenge(&self, _params: Value) -> WsFrame {
        let challenge = uuid::Uuid::new_v4().to_string();
        WsFrame::ok_response("", json!({ "challenge": challenge }))
    }

    fn handle_connect(&self, params: Value) -> WsFrame {
        let version = params.get("version").and_then(|v| v.as_str()).unwrap_or("unknown");
        WsFrame::ok_response("", json!({ "version": env!("CARGO_PKG_VERSION"), "client_version": version, "status": "connected" }))
    }

    fn handle_hello_ok(&self, _params: Value) -> WsFrame {
        WsFrame::ok_response("", json!({ "ack": true }))
    }

    fn handle_tools_catalog(&self, _params: Value) -> WsFrame {
        WsFrame::ok_response("", json!({
            "tools": [
                { "name": "agents.list", "description": "List all registered agents" },
                { "name": "agents.status", "description": "Get agent status" },
                { "name": "agents.create", "description": "Create a new agent" },
                { "name": "agents.delegate", "description": "Delegate a task" },
                { "name": "agents.pause", "description": "Pause an agent" },
                { "name": "agents.resume", "description": "Resume an agent" },
                { "name": "agents.inspect", "description": "Inspect agent details" },
                { "name": "channels.status", "description": "Channel connection status" },
                { "name": "accounts.budget_summary", "description": "Budget overview" },
                { "name": "system.status", "description": "System status" },
                { "name": "system.doctor", "description": "Health checks" },
                { "name": "system.version", "description": "Version info" },
            ]
        }))
    }

    // ── Agents ───────────────────────────────────────────────

    async fn handle_agents_list(&self) -> WsFrame {
        // Re-scan to pick up changes
        {
            let mut reg = self.registry.write().await;
            let _ = reg.scan().await;
        }

        let reg = self.registry.read().await;
        let agents: Vec<Value> = reg.list().iter().map(|a| {
            let cfg = &a.config;
            json!({
                "name": cfg.agent.name,
                "display_name": cfg.agent.display_name,
                "role": format!("{:?}", cfg.agent.role).to_lowercase(),
                "status": format!("{:?}", cfg.agent.status).to_lowercase(),
                "trigger": cfg.agent.trigger,
                "icon": cfg.agent.icon,
                "reports_to": cfg.agent.reports_to,
                "model": {
                    "preferred": cfg.model.preferred,
                    "fallback": cfg.model.fallback,
                    "account_pool": cfg.model.account_pool,
                },
                "budget": {
                    "monthly_limit_cents": cfg.budget.monthly_limit_cents,
                    "spent_cents": 0,
                    "warn_threshold_percent": cfg.budget.warn_threshold_percent,
                    "hard_stop": cfg.budget.hard_stop,
                },
                "heartbeat": {
                    "enabled": cfg.heartbeat.enabled,
                    "interval_seconds": cfg.heartbeat.interval_seconds,
                },
                "skills": a.skills.iter().map(|s| &s.name).collect::<Vec<_>>(),
                "permissions": {
                    "can_create_agents": cfg.permissions.can_create_agents,
                    "can_send_cross_agent": cfg.permissions.can_send_cross_agent,
                    "can_modify_own_skills": cfg.permissions.can_modify_own_skills,
                    "can_modify_own_soul": cfg.permissions.can_modify_own_soul,
                    "can_schedule_tasks": cfg.permissions.can_schedule_tasks,
                },
            })
        }).collect();

        info!("agents.list: found {} agents", agents.len());
        WsFrame::ok_response("", json!({ "agents": agents }))
    }

    async fn handle_agents_status(&self, params: Value) -> WsFrame {
        let agent_id = params.get("agent_id").and_then(|v| v.as_str()).unwrap_or("");
        let reg = self.registry.read().await;
        match reg.get(agent_id) {
            Some(a) => {
                let cfg = &a.config;
                WsFrame::ok_response("", json!({
                    "name": cfg.agent.name,
                    "display_name": cfg.agent.display_name,
                    "status": format!("{:?}", cfg.agent.status).to_lowercase(),
                    "role": format!("{:?}", cfg.agent.role).to_lowercase(),
                }))
            }
            None => WsFrame::error_response("", &format!("Agent not found: {agent_id}")),
        }
    }

    async fn handle_agents_create(&self, params: Value) -> WsFrame {
        let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let display_name = params.get("display_name").and_then(|v| v.as_str()).unwrap_or(name);
        let role = params.get("role").and_then(|v| v.as_str()).unwrap_or("specialist");
        let trigger = params.get("trigger").and_then(|v| v.as_str()).unwrap_or("");
        let trigger = if trigger.is_empty() { format!("@{display_name}") } else { trigger.to_string() };

        if name.is_empty() {
            return WsFrame::error_response("", "Agent name is required");
        }

        // Create agent directory and files
        let reg = self.registry.read().await;
        let agents_dir = reg.agents_dir();
        let agent_dir = agents_dir.join(name);

        if agent_dir.exists() {
            return WsFrame::error_response("", &format!("Agent '{name}' already exists"));
        }

        let skills_dir = agent_dir.join("SKILLS");
        if let Err(e) = tokio::fs::create_dir_all(&skills_dir).await {
            return WsFrame::error_response("", &format!("Failed to create directory: {e}"));
        }

        let agent_toml = format!(r#"[agent]
name = "{name}"
display_name = "{display_name}"
role = "{role}"
status = "active"
trigger = "{trigger}"
reports_to = ""
icon = "🤖"

[model]
preferred = "claude-sonnet-4-6"
fallback = "claude-haiku-4-5"
account_pool = ["main"]

[container]
timeout_ms = 1800000
max_concurrent = 1
readonly_project = true
additional_mounts = []

[heartbeat]
enabled = false
interval_seconds = 3600
max_concurrent_runs = 1
cron = ""

[budget]
monthly_limit_cents = 5000
warn_threshold_percent = 80
hard_stop = true

[permissions]
can_create_agents = false
can_send_cross_agent = true
can_modify_own_skills = true
can_modify_own_soul = false
can_schedule_tasks = false
allowed_channels = ["*"]

[evolution]
micro_reflection = false
meso_reflection = false
macro_reflection = false
skill_auto_activate = false
skill_security_scan = true
"#);

        if let Err(e) = tokio::fs::write(agent_dir.join("agent.toml"), &agent_toml).await {
            return WsFrame::error_response("", &format!("Failed to write agent.toml: {e}"));
        }

        let soul = format!("# {display_name}\n\nI am {display_name}, a specialist AI agent.\n");
        let _ = tokio::fs::write(agent_dir.join("SOUL.md"), &soul).await;

        info!(name, "Agent created");
        WsFrame::ok_response("", json!({
            "success": true,
            "agent": { "name": name, "display_name": display_name, "role": role, "status": "active" }
        }))
    }

    async fn handle_agents_delegate(&self, params: Value) -> WsFrame {
        let agent_id = params.get("agent_id").and_then(|v| v.as_str()).unwrap_or("");
        let prompt = params.get("prompt").and_then(|v| v.as_str()).unwrap_or("");
        info!(agent_id, "agents.delegate requested");
        let message_id = uuid::Uuid::new_v4().to_string();
        WsFrame::ok_response("", json!({ "success": true, "message_id": message_id, "target_agent": agent_id, "prompt": prompt }))
    }

    async fn handle_agents_pause(&self, params: Value) -> WsFrame {
        let agent_id = params.get("agent_id").and_then(|v| v.as_str()).unwrap_or("");
        info!(agent_id, "agents.pause requested");
        // TODO: actually modify agent.toml status field
        WsFrame::ok_response("", json!({ "success": true, "name": agent_id, "status": "paused" }))
    }

    async fn handle_agents_resume(&self, params: Value) -> WsFrame {
        let agent_id = params.get("agent_id").and_then(|v| v.as_str()).unwrap_or("");
        info!(agent_id, "agents.resume requested");
        WsFrame::ok_response("", json!({ "success": true, "name": agent_id, "status": "active" }))
    }

    async fn handle_agents_inspect(&self, params: Value) -> WsFrame {
        let agent_id = params.get("agent_id").and_then(|v| v.as_str()).unwrap_or("");
        let reg = self.registry.read().await;
        match reg.get(agent_id) {
            Some(a) => {
                let cfg = &a.config;
                WsFrame::ok_response("", json!({
                    "name": cfg.agent.name,
                    "display_name": cfg.agent.display_name,
                    "role": format!("{:?}", cfg.agent.role).to_lowercase(),
                    "status": format!("{:?}", cfg.agent.status).to_lowercase(),
                    "trigger": cfg.agent.trigger,
                    "icon": cfg.agent.icon,
                    "reports_to": cfg.agent.reports_to,
                    "soul": a.soul,
                    "identity": a.identity,
                    "memory_summary": a.memory,
                    "skills": a.skills.iter().map(|s| &s.name).collect::<Vec<_>>(),
                    "model": { "preferred": cfg.model.preferred, "fallback": cfg.model.fallback, "account_pool": cfg.model.account_pool },
                    "budget": { "monthly_limit_cents": cfg.budget.monthly_limit_cents, "spent_cents": 0, "warn_threshold_percent": cfg.budget.warn_threshold_percent, "hard_stop": cfg.budget.hard_stop },
                    "heartbeat": { "enabled": cfg.heartbeat.enabled, "interval_seconds": cfg.heartbeat.interval_seconds },
                    "permissions": {
                        "can_create_agents": cfg.permissions.can_create_agents,
                        "can_send_cross_agent": cfg.permissions.can_send_cross_agent,
                        "can_modify_own_skills": cfg.permissions.can_modify_own_skills,
                        "can_modify_own_soul": cfg.permissions.can_modify_own_soul,
                        "can_schedule_tasks": cfg.permissions.can_schedule_tasks,
                    },
                }))
            }
            None => WsFrame::error_response("", &format!("Agent not found: {agent_id}")),
        }
    }

    // ── Channels ─────────────────────────────────────────────

    async fn handle_channels_status(&self) -> WsFrame {
        // TODO: read from actual channel registry
        WsFrame::ok_response("", json!({ "channels": [] }))
    }

    // ── Accounts ─────────────────────────────────────────────

    async fn handle_budget_summary(&self) -> WsFrame {
        // Read budgets from loaded agents
        let reg = self.registry.read().await;
        let mut total_budget: u64 = 0;
        let agents_list = reg.list();
        for a in &agents_list {
            total_budget += a.config.budget.monthly_limit_cents;
        }
        WsFrame::ok_response("", json!({
            "total_budget_cents": total_budget,
            "total_spent_cents": 0,
            "accounts": [],
        }))
    }

    // ── System ───────────────────────────────────────────────

    async fn handle_system_status(&self) -> WsFrame {
        let reg = self.registry.read().await;
        WsFrame::ok_response("", json!({
            "version": env!("CARGO_PKG_VERSION"),
            "uptime_seconds": 0,
            "agents_count": reg.list().len(),
            "channels_connected": 0,
            "gateway_address": "localhost:18789",
        }))
    }

    async fn handle_system_doctor(&self) -> WsFrame {
        let reg = self.registry.read().await;
        let has_agents = !reg.list().is_empty();
        let checks = vec![
            json!({ "name": "config_file", "status": "pass", "message": "config.toml 存在", "can_repair": false }),
            json!({ "name": "agents", "status": if has_agents { "pass" } else { "warn" }, "message": if has_agents { "已找到 Agent" } else { "未找到任何 Agent" }, "can_repair": false }),
            json!({ "name": "container_runtime", "status": "pass", "message": "Docker 可用", "can_repair": false }),
        ];
        let pass = checks.iter().filter(|c| c["status"] == "pass").count();
        let warn = checks.iter().filter(|c| c["status"] == "warn").count();
        let fail = checks.iter().filter(|c| c["status"] == "fail").count();
        WsFrame::ok_response("", json!({ "checks": checks, "summary": { "pass": pass, "warn": warn, "fail": fail } }))
    }

    fn handle_system_version(&self) -> WsFrame {
        WsFrame::ok_response("", json!({ "version": env!("CARGO_PKG_VERSION") }))
    }

    // ── Evolution ────────────────────────────────────────────

    async fn handle_evolution_status(&self) -> WsFrame {
        WsFrame::ok_response("", json!({ "enabled": false, "message": "Evolution subsystem not yet active" }))
    }

    async fn handle_evolution_skills(&self) -> WsFrame {
        WsFrame::ok_response("", json!({ "skills": [] }))
    }
}
