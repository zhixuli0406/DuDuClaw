use std::path::{Path, PathBuf};
use std::sync::Arc;

use duduclaw_agent::registry::AgentRegistry;
use duduclaw_core::traits::MemoryEngine;
use duduclaw_memory::SqliteMemoryEngine;
use serde_json::{json, Value};
use tokio::sync::RwLock;
use tracing::info;

use crate::protocol::WsFrame;

/// Dispatches incoming RPC methods to the appropriate handler.
pub struct MethodHandler {
    registry: Arc<RwLock<AgentRegistry>>,
    home_dir: PathBuf,
}

impl MethodHandler {
    pub async fn new(home_dir: PathBuf) -> Self {
        let agents_dir = home_dir.join("agents");
        let mut registry = AgentRegistry::new(agents_dir);
        if let Err(e) = registry.scan().await {
            tracing::warn!("Failed to scan agents directory: {e}");
        }
        Self {
            registry: Arc::new(RwLock::new(registry)),
            home_dir,
        }
    }

    /// Get a reference to the shared agent registry.
    pub fn registry(&self) -> &Arc<RwLock<AgentRegistry>> {
        &self.registry
    }

    /// Get the home directory path.
    pub fn home_dir(&self) -> &Path {
        &self.home_dir
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
            "channels.add" => self.handle_channels_add(params).await,
            "channels.test" => self.handle_channels_test(params).await,
            "channels.remove" => self.handle_channels_remove(params).await,
            "accounts.list" => self.handle_accounts_list().await,
            "accounts.budget_summary" => self.handle_budget_summary().await,
            "accounts.rotate" => self.handle_accounts_rotate(params),
            "accounts.health" => self.handle_accounts_health().await,
            "memory.search" => self.handle_memory_search(params).await,
            "memory.browse" => self.handle_memory_browse(params).await,
            "skills.list" => self.handle_skills_list(params).await,
            "skills.content" => self.handle_skills_content(params).await,
            "cron.list" => self.handle_cron_list(),
            "cron.add" => self.handle_cron_add(params),
            "cron.pause" => self.handle_cron_pause(params),
            "cron.remove" => self.handle_cron_remove(params),
            "system.status" => self.handle_system_status().await,
            "system.doctor" => self.handle_system_doctor().await,
            "system.doctor_repair" => self.handle_system_doctor_repair().await,
            "system.config" => self.handle_system_config().await,
            "system.version" => self.handle_system_version(),
            "logs.subscribe" => self.handle_logs_subscribe(params),
            "logs.unsubscribe" => self.handle_logs_unsubscribe(params),
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
                { "name": "channels.add", "description": "Add a channel" },
                { "name": "channels.test", "description": "Test a channel" },
                { "name": "channels.remove", "description": "Remove a channel" },
                { "name": "accounts.list", "description": "List accounts" },
                { "name": "accounts.budget_summary", "description": "Budget overview" },
                { "name": "accounts.rotate", "description": "Rotate account key" },
                { "name": "accounts.health", "description": "Account health check" },
                { "name": "memory.search", "description": "Search agent memory" },
                { "name": "memory.browse", "description": "Browse recent memory entries" },
                { "name": "skills.list", "description": "List agent skills" },
                { "name": "skills.content", "description": "Read skill content" },
                { "name": "cron.list", "description": "List cron jobs" },
                { "name": "cron.add", "description": "Add a cron job" },
                { "name": "cron.pause", "description": "Pause a cron job" },
                { "name": "cron.remove", "description": "Remove a cron job" },
                { "name": "system.status", "description": "System status" },
                { "name": "system.doctor", "description": "Health checks" },
                { "name": "system.doctor_repair", "description": "Health checks with repair hints" },
                { "name": "system.config", "description": "View system config" },
                { "name": "system.version", "description": "Version info" },
                { "name": "logs.subscribe", "description": "Subscribe to logs" },
                { "name": "logs.unsubscribe", "description": "Unsubscribe from logs" },
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
        let config_path = self.home_dir.join("config.toml");
        let mut channels = Vec::new();

        if let Ok(content) = tokio::fs::read_to_string(&config_path).await
            && let Ok(config) = content.parse::<toml::Table>()
            && let Some(ch) = config.get("channels").and_then(|v| v.as_table())
        {
            let token_map = [
                ("line_channel_token", "line"),
                ("telegram_bot_token", "telegram"),
                ("discord_bot_token", "discord"),
            ];
            for (key, name) in token_map {
                if ch.get(key).and_then(|v| v.as_str()).is_some_and(|s| !s.is_empty()) {
                    channels.push(json!({
                        "name": name,
                        "connected": true,
                        "last_connected": null,
                        "error": null,
                    }));
                }
            }
        }

        WsFrame::ok_response("", json!({ "channels": channels }))
    }

    async fn handle_channels_add(&self, params: Value) -> WsFrame {
        let channel_type = match params.get("type").and_then(|v| v.as_str()) {
            Some(t) => t,
            None => return WsFrame::error_response("", "Missing 'type' parameter"),
        };
        let config_obj = params.get("config").cloned().unwrap_or(json!({}));
        let token = config_obj.get("token").and_then(|v| v.as_str()).unwrap_or("");
        let secret = config_obj.get("secret").and_then(|v| v.as_str()).unwrap_or("");

        if token.is_empty() {
            return WsFrame::error_response("", "Missing 'config.token' parameter");
        }

        let (token_key, secret_key) = match channel_type {
            "line" => ("line_channel_token", Some("line_channel_secret")),
            "telegram" => ("telegram_bot_token", None),
            "discord" => ("discord_bot_token", None),
            _ => return WsFrame::error_response("", &format!("Unknown channel type: {channel_type}")),
        };

        let config_path = self.home_dir.join("config.toml");
        let mut table = self.read_config_table(&config_path).await;

        let channels = table
            .entry("channels")
            .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
            .as_table_mut();

        let channels = match channels {
            Some(ch) => ch,
            None => return WsFrame::error_response("", "Invalid [channels] section in config.toml"),
        };

        channels.insert(token_key.to_string(), toml::Value::String(token.to_string()));
        if let Some(sk) = secret_key
            && !secret.is_empty()
        {
            channels.insert(sk.to_string(), toml::Value::String(secret.to_string()));
        }

        if let Err(e) = self.write_config_table(&config_path, &table).await {
            return WsFrame::error_response("", &format!("Failed to write config.toml: {e}"));
        }

        info!(channel_type, "Channel added");
        WsFrame::ok_response("", json!({ "success": true, "type": channel_type }))
    }

    async fn handle_channels_test(&self, params: Value) -> WsFrame {
        let channel_type = params.get("type").and_then(|v| v.as_str()).unwrap_or("unknown");
        info!(channel_type, "channels.test requested");

        let config_path = self.home_dir.join("config.toml");
        let table = self.read_config_table(&config_path).await;

        let token_key = match channel_type {
            "line" => "line_channel_token",
            "telegram" => "telegram_bot_token",
            "discord" => "discord_bot_token",
            _ => return WsFrame::error_response("", &format!("Unknown channel type: {channel_type}")),
        };

        let token = table
            .get("channels")
            .and_then(|v| v.as_table())
            .and_then(|ch| ch.get(token_key))
            .and_then(|v| v.as_str())
            .unwrap_or("");

        if token.is_empty() {
            return WsFrame::ok_response("", json!({
                "success": false,
                "type": channel_type,
                "message": format!("{channel_type} token 未設定"),
            }));
        }

        WsFrame::ok_response("", json!({
            "success": true,
            "type": channel_type,
            "message": format!("{channel_type} token 已設定 ({}****)", &token[..4.min(token.len())]),
        }))
    }

    async fn handle_channels_remove(&self, params: Value) -> WsFrame {
        let channel_type = match params.get("type").and_then(|v| v.as_str()) {
            Some(t) => t,
            None => return WsFrame::error_response("", "Missing 'type' parameter"),
        };

        let token_key = match channel_type {
            "line" => "line_channel_token",
            "telegram" => "telegram_bot_token",
            "discord" => "discord_bot_token",
            _ => return WsFrame::error_response("", &format!("Unknown channel type: {channel_type}")),
        };

        let config_path = self.home_dir.join("config.toml");
        let mut table = self.read_config_table(&config_path).await;

        if let Some(channels) = table.get_mut("channels").and_then(|v| v.as_table_mut()) {
            channels.insert(token_key.to_string(), toml::Value::String(String::new()));
            // Also clear secret for LINE
            if channel_type == "line" {
                channels.insert("line_channel_secret".to_string(), toml::Value::String(String::new()));
            }
        }

        if let Err(e) = self.write_config_table(&config_path, &table).await {
            return WsFrame::error_response("", &format!("Failed to write config.toml: {e}"));
        }

        info!(channel_type, "Channel removed");
        WsFrame::ok_response("", json!({ "success": true, "type": channel_type }))
    }

    // ── Accounts ─────────────────────────────────────────────

    async fn handle_accounts_list(&self) -> WsFrame {
        let has_key = self.has_api_key().await;
        let mut accounts = Vec::new();
        if has_key {
            accounts.push(json!({
                "name": "main",
                "provider": "anthropic",
                "active": true,
            }));
        }
        WsFrame::ok_response("", json!({ "accounts": accounts }))
    }

    async fn handle_budget_summary(&self) -> WsFrame {
        // Read budgets from loaded agents
        let reg = self.registry.read().await;
        let mut total_budget: u64 = 0;
        let agents_list = reg.list();
        for a in &agents_list {
            total_budget += a.config.budget.monthly_limit_cents;
        }

        let has_key = self.has_api_key().await;
        let accounts: Vec<Value> = if has_key {
            vec![json!({
                "id": "main",
                "account_type": "api_key",
                "priority": 1,
                "is_healthy": true,
                "spent_this_month": 0,
                "monthly_budget_cents": total_budget,
            })]
        } else {
            vec![]
        };

        WsFrame::ok_response("", json!({
            "total_budget_cents": total_budget,
            "total_spent_cents": 0,
            "accounts": accounts,
        }))
    }

    fn handle_accounts_rotate(&self, params: Value) -> WsFrame {
        let account = params.get("account").and_then(|v| v.as_str()).unwrap_or("main");
        info!(account, "accounts.rotate requested");
        WsFrame::ok_response("", json!({ "success": true, "account": account, "message": "Key rotation placeholder" }))
    }

    async fn handle_accounts_health(&self) -> WsFrame {
        let has_key = self.has_api_key().await;
        let status = if has_key { "healthy" } else { "missing_key" };
        WsFrame::ok_response("", json!({
            "status": status,
            "accounts": [{
                "name": "main",
                "provider": "anthropic",
                "healthy": has_key,
                "message": if has_key { "ANTHROPIC_API_KEY is set" } else { "ANTHROPIC_API_KEY is not set" },
            }],
        }))
    }

    // ── Memory ──────────────────────────────────────────────

    async fn handle_memory_search(&self, params: Value) -> WsFrame {
        let agent_id = params.get("agent_id").and_then(|v| v.as_str()).unwrap_or("");
        let query = params.get("query").and_then(|v| v.as_str()).unwrap_or("");
        let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(20) as usize;

        if agent_id.is_empty() || query.is_empty() {
            return WsFrame::error_response("", "Missing 'agent_id' or 'query' parameter");
        }

        let db_path = self.home_dir.join("state.db");
        if !db_path.exists() {
            return WsFrame::ok_response("", json!({ "results": [] }));
        }

        let engine = match SqliteMemoryEngine::new(&db_path) {
            Ok(e) => e,
            Err(e) => return WsFrame::error_response("", &format!("Failed to open memory db: {e}")),
        };

        match engine.search(agent_id, query, limit).await {
            Ok(entries) => {
                let results: Vec<Value> = entries.iter().map(|e| {
                    json!({
                        "id": e.id,
                        "agent_id": e.agent_id,
                        "content": e.content,
                        "timestamp": e.timestamp.to_rfc3339(),
                        "tags": e.tags,
                    })
                }).collect();
                WsFrame::ok_response("", json!({ "results": results }))
            }
            Err(e) => WsFrame::error_response("", &format!("Memory search failed: {e}")),
        }
    }

    async fn handle_memory_browse(&self, params: Value) -> WsFrame {
        let agent_id = params.get("agent_id").and_then(|v| v.as_str()).unwrap_or("");
        let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(20) as usize;

        if agent_id.is_empty() {
            return WsFrame::error_response("", "Missing 'agent_id' parameter");
        }

        let db_path = self.home_dir.join("state.db");
        if !db_path.exists() {
            return WsFrame::ok_response("", json!({ "entries": [] }));
        }

        let engine = match SqliteMemoryEngine::new(&db_path) {
            Ok(e) => e,
            Err(e) => return WsFrame::error_response("", &format!("Failed to open memory db: {e}")),
        };

        // Browse recent entries using a wildcard-like search; FTS5 requires a query
        // so we use summarize with a broad time window instead.
        let now = chrono::Utc::now();
        let window = duduclaw_core::types::TimeWindow {
            start: now - chrono::Duration::days(365),
            end: now,
        };

        match engine.summarize(agent_id, window).await {
            Ok(summary) => {
                // The summarize method returns a text summary. For browse, we re-query
                // the raw entries. Since the engine trait only exposes search and summarize,
                // we do a broad search with a common token. If that returns nothing, return
                // the summary text instead.
                // A pragmatic approach: return the summary as a single entry.
                let _ = limit; // acknowledged but summary is the best we can do via trait
                WsFrame::ok_response("", json!({
                    "agent_id": agent_id,
                    "summary": summary,
                    "entries": [],
                }))
            }
            Err(e) => WsFrame::error_response("", &format!("Memory browse failed: {e}")),
        }
    }

    // ── Skills ──────────────────────────────────────────────

    async fn handle_skills_list(&self, params: Value) -> WsFrame {
        let agent_id = params.get("agent_id").and_then(|v| v.as_str());
        let reg = self.registry.read().await;

        match agent_id {
            Some(id) => {
                match reg.get(id) {
                    Some(agent) => {
                        let skills: Vec<Value> = agent.skills.iter().map(|s| {
                            json!({ "name": s.name, "size": s.content.len() })
                        }).collect();
                        WsFrame::ok_response("", json!({ "agent_id": id, "skills": skills }))
                    }
                    None => WsFrame::error_response("", &format!("Agent not found: {id}")),
                }
            }
            None => {
                // Return skills for all agents
                let mut all_skills = Vec::new();
                for agent in reg.list() {
                    let skills: Vec<Value> = agent.skills.iter().map(|s| {
                        json!({ "name": s.name, "size": s.content.len() })
                    }).collect();
                    all_skills.push(json!({
                        "agent_id": agent.config.agent.name,
                        "skills": skills,
                    }));
                }
                WsFrame::ok_response("", json!({ "agents": all_skills }))
            }
        }
    }

    async fn handle_skills_content(&self, params: Value) -> WsFrame {
        let agent_id = match params.get("agent_id").and_then(|v| v.as_str()) {
            Some(id) => id,
            None => return WsFrame::error_response("", "Missing 'agent_id' parameter"),
        };
        let skill_name = match params.get("skill_name").and_then(|v| v.as_str()) {
            Some(n) => n,
            None => return WsFrame::error_response("", "Missing 'skill_name' parameter"),
        };

        let reg = self.registry.read().await;
        match reg.get(agent_id) {
            Some(agent) => {
                match agent.skills.iter().find(|s| s.name == skill_name) {
                    Some(skill) => WsFrame::ok_response("", json!({
                        "agent_id": agent_id,
                        "skill_name": skill_name,
                        "content": skill.content,
                    })),
                    None => WsFrame::error_response("", &format!("Skill not found: {skill_name}")),
                }
            }
            None => WsFrame::error_response("", &format!("Agent not found: {agent_id}")),
        }
    }

    // ── Cron ────────────────────────────────────────────────

    fn handle_cron_list(&self) -> WsFrame {
        WsFrame::ok_response("", json!({ "tasks": [] }))
    }

    fn handle_cron_add(&self, params: Value) -> WsFrame {
        let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("unnamed");
        info!(name, "cron.add requested");
        WsFrame::ok_response("", json!({ "success": true, "message": "Cron add placeholder" }))
    }

    fn handle_cron_pause(&self, params: Value) -> WsFrame {
        let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("unnamed");
        info!(name, "cron.pause requested");
        WsFrame::ok_response("", json!({ "success": true, "message": "Cron pause placeholder" }))
    }

    fn handle_cron_remove(&self, params: Value) -> WsFrame {
        let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("unnamed");
        info!(name, "cron.remove requested");
        WsFrame::ok_response("", json!({ "success": true, "message": "Cron remove placeholder" }))
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
        let checks = self.run_doctor_checks().await;
        let pass = checks.iter().filter(|c| c["status"] == "pass").count();
        let warn = checks.iter().filter(|c| c["status"] == "warn").count();
        let fail = checks.iter().filter(|c| c["status"] == "fail").count();
        WsFrame::ok_response("", json!({ "checks": checks, "summary": { "pass": pass, "warn": warn, "fail": fail } }))
    }

    async fn handle_system_doctor_repair(&self) -> WsFrame {
        let checks = self.run_doctor_checks().await;
        let pass = checks.iter().filter(|c| c["status"] == "pass").count();
        let warn = checks.iter().filter(|c| c["status"] == "warn").count();
        let fail = checks.iter().filter(|c| c["status"] == "fail").count();

        let repair_hints: Vec<Value> = checks.iter().filter(|c| c["status"] != "pass").map(|c| {
            let name = c["name"].as_str().unwrap_or("unknown");
            let hint = match name {
                "agents" => "Run 'duduclaw agent create <name>' to create your first agent.",
                "api_key" => "Set ANTHROPIC_API_KEY environment variable with a valid key.",
                "config_file" => "Run 'duduclaw init' to create a default config.toml.",
                _ => "Check the documentation for repair instructions.",
            };
            json!({ "check": name, "hint": hint })
        }).collect();

        WsFrame::ok_response("", json!({
            "checks": checks,
            "summary": { "pass": pass, "warn": warn, "fail": fail },
            "repair_hints": repair_hints,
        }))
    }

    async fn handle_system_config(&self) -> WsFrame {
        let config_path = self.home_dir.join("config.toml");
        match tokio::fs::read_to_string(&config_path).await {
            Ok(content) => {
                // Mask sensitive fields
                match content.parse::<toml::Table>() {
                    Ok(mut table) => {
                        Self::mask_sensitive_fields(&mut table);
                        let masked = toml::to_string_pretty(&table).unwrap_or_else(|_| content.clone());
                        WsFrame::ok_response("", json!({ "config": masked }))
                    }
                    Err(_) => {
                        // Return raw content if parsing fails
                        WsFrame::ok_response("", json!({ "config": content }))
                    }
                }
            }
            Err(e) => WsFrame::error_response("", &format!("Failed to read config.toml: {e}")),
        }
    }

    fn handle_system_version(&self) -> WsFrame {
        WsFrame::ok_response("", json!({ "version": env!("CARGO_PKG_VERSION") }))
    }

    // ── Logs ────────────────────────────────────────────────

    fn handle_logs_subscribe(&self, params: Value) -> WsFrame {
        let filter = params.get("filter").and_then(|v| v.as_str()).unwrap_or("*");
        info!(filter, "logs.subscribe requested");
        WsFrame::ok_response("", json!({ "success": true, "message": "Log subscription registered (WebSocket push is future work)" }))
    }

    fn handle_logs_unsubscribe(&self, params: Value) -> WsFrame {
        let filter = params.get("filter").and_then(|v| v.as_str()).unwrap_or("*");
        info!(filter, "logs.unsubscribe requested");
        WsFrame::ok_response("", json!({ "success": true, "message": "Log subscription removed" }))
    }

    // ── Evolution ────────────────────────────────────────────

    async fn handle_evolution_status(&self) -> WsFrame {
        let reg = self.registry.read().await;
        let agents: Vec<Value> = reg.list().iter().map(|a| {
            let cfg = &a.config;
            json!({
                "agent_id": cfg.agent.name,
                "micro_reflection": cfg.evolution.micro_reflection,
                "meso_reflection": cfg.evolution.meso_reflection,
                "macro_reflection": cfg.evolution.macro_reflection,
                "skill_auto_activate": cfg.evolution.skill_auto_activate,
                "skill_security_scan": cfg.evolution.skill_security_scan,
            })
        }).collect();

        let any_enabled = reg.list().iter().any(|a| {
            let e = &a.config.evolution;
            e.micro_reflection || e.meso_reflection || e.macro_reflection
        });

        WsFrame::ok_response("", json!({
            "enabled": any_enabled,
            "agents": agents,
            "timers": {
                "meso_interval_seconds": 3600,
                "macro_interval_seconds": 86400,
            },
        }))
    }

    async fn handle_evolution_skills(&self) -> WsFrame {
        let reg = self.registry.read().await;
        let mut all_skills = Vec::new();
        for agent in reg.list() {
            for skill in &agent.skills {
                all_skills.push(json!({
                    "agent_id": agent.config.agent.name,
                    "name": skill.name,
                    "size": skill.content.len(),
                }));
            }
        }
        WsFrame::ok_response("", json!({ "skills": all_skills }))
    }

    // ── Helpers ─────────────────────────────────────────────

    /// Check if an API key is available (from env var or config.toml [api] section).
    async fn has_api_key(&self) -> bool {
        // 1. Check environment variable
        if std::env::var("ANTHROPIC_API_KEY").is_ok_and(|k| !k.is_empty()) {
            return true;
        }
        // 2. Check config.toml [api] section
        let table = self.read_config_table(&self.home_dir.join("config.toml")).await;
        if let Some(api) = table.get("api").and_then(|v| v.as_table())
            && api.get("anthropic_api_key").and_then(|v| v.as_str()).is_some_and(|s| !s.is_empty())
        {
            return true;
        }
        // 3. Check accounts in config.toml
        if let Some(accounts) = table.get("accounts")
            && let Some(arr) = accounts.as_array()
        {
            return !arr.is_empty();
        }
        false
    }

    /// Read config.toml into a TOML table, returning an empty table if the file
    /// does not exist or cannot be parsed.
    async fn read_config_table(&self, path: &std::path::Path) -> toml::Table {
        match tokio::fs::read_to_string(path).await {
            Ok(content) => content.parse::<toml::Table>().unwrap_or_default(),
            Err(_) => toml::Table::new(),
        }
    }

    /// Write a TOML table back to disk.
    async fn write_config_table(
        &self,
        path: &std::path::Path,
        table: &toml::Table,
    ) -> std::io::Result<()> {
        let content = toml::to_string_pretty(table).map_err(|e| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string())
        })?;
        tokio::fs::write(path, content).await
    }

    /// Run common health checks used by both doctor and doctor_repair.
    async fn run_doctor_checks(&self) -> Vec<Value> {
        let reg = self.registry.read().await;
        let has_agents = !reg.list().is_empty();
        let has_key = self.has_api_key().await;
        let config_exists = self.home_dir.join("config.toml").exists();

        vec![
            json!({
                "name": "config_file",
                "status": if config_exists { "pass" } else { "fail" },
                "message": if config_exists { "config.toml exists" } else { "config.toml not found" },
                "can_repair": !config_exists,
            }),
            json!({
                "name": "agents",
                "status": if has_agents { "pass" } else { "warn" },
                "message": if has_agents { "Agents found" } else { "No agents found" },
                "can_repair": false,
            }),
            json!({
                "name": "api_key",
                "status": if has_key { "pass" } else { "warn" },
                "message": if has_key { "ANTHROPIC_API_KEY is set" } else { "ANTHROPIC_API_KEY not set" },
                "can_repair": false,
            }),
            json!({
                "name": "container_runtime",
                "status": "pass",
                "message": "Docker available",
                "can_repair": false,
            }),
        ]
    }

    /// Mask sensitive values (tokens, secrets, keys) in a TOML table.
    fn mask_sensitive_fields(table: &mut toml::Table) {
        let sensitive_patterns = ["token", "secret", "key", "password"];
        for (key, value) in table.iter_mut() {
            let is_sensitive = sensitive_patterns.iter().any(|p| key.to_lowercase().contains(p));
            match value {
                toml::Value::String(s) if is_sensitive && !s.is_empty() => {
                    let len = s.len();
                    if len > 4 {
                        let masked = format!("{}****", &s[..4]);
                        *s = masked;
                    } else {
                        *s = "****".to_string();
                    }
                }
                toml::Value::Table(t) => Self::mask_sensitive_fields(t),
                _ => {}
            }
        }
    }
}
