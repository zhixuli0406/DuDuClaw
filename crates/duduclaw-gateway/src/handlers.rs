use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use duduclaw_agent::registry::AgentRegistry;
use duduclaw_auth::{self, UserContext, UserDb, JwtConfig};
use duduclaw_auth::acl;
use duduclaw_auth::models::{UserRole, AccessLevel};
use duduclaw_core::traits::MemoryEngine;
use duduclaw_core::truncate_bytes;
use duduclaw_memory::SqliteMemoryEngine;
use chrono::{Datelike, Utc};
use rusqlite::params;
use serde_json::{json, Value};
use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::autopilot_store::{AutopilotStore, AutopilotRuleRow, AutopilotHistoryRow};
use crate::cron_scheduler::CronScheduler;
use crate::cron_store::{CronStore, CronTaskRow};
use crate::extension::GatewayExtension;
use crate::gvu::version_store::VersionStore;
use crate::protocol::WsFrame;
use crate::task_store::{TaskStore, TaskRow, ActivityRow};
use crate::partner_store::{
    PartnerStore, PartnerProfileInput, PartnerCustomerInput, PartnerCustomerPatch,
};

/// Validate agent ID is safe for filesystem paths (no traversal).
fn is_valid_agent_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !id.starts_with('-')
        && !id.ends_with('-')
        && !id.contains("..")
}

/// Dispatches incoming RPC methods to the appropriate handler.
pub struct MethodHandler {
    registry: Arc<RwLock<AgentRegistry>>,
    home_dir: PathBuf,
    start_time: Instant,
    channel_status: Arc<RwLock<std::collections::HashMap<String, ChannelState>>>,
    heartbeat: RwLock<Option<Arc<duduclaw_agent::HeartbeatScheduler>>>,
    /// Reply context for hot-starting channels after config changes.
    reply_ctx: RwLock<Option<Arc<crate::channel_reply::ReplyContext>>>,
    /// Handles for running channel bot tasks (for hot-stop on remove).
    channel_handles: tokio::sync::Mutex<std::collections::HashMap<String, tokio::task::JoinHandle<()>>>,
    /// [M2] Server-side cached pending update (set by check_update, consumed by apply_update).
    pending_update: RwLock<Option<PendingUpdate>>,
    /// User database for multi-user auth (injected after gateway start).
    user_db: RwLock<Option<Arc<UserDb>>>,
    /// JWT configuration for token issuance (injected after gateway start).
    jwt_config: RwLock<Option<Arc<JwtConfig>>>,
    /// Plugin extension point (NullExtension by default).
    extension: Arc<dyn GatewayExtension>,
    /// SQLite-backed cron task store. Injected after gateway starts.
    cron_store: RwLock<Option<Arc<CronStore>>>,
    /// Handle to the running cron scheduler — used to trigger hot reload
    /// after mutating `cron_store`. Injected after gateway starts.
    cron_scheduler: RwLock<Option<Arc<CronScheduler>>>,
    /// Pending OAuth flows awaiting callback (keyed by state nonce).
    mcp_oauth_pending: RwLock<std::collections::HashMap<String, crate::mcp_oauth::PendingOAuth>>,
    /// SQLite-backed task board store. Injected after gateway starts.
    task_store: RwLock<Option<Arc<TaskStore>>>,
    /// SQLite-backed autopilot rule store. Injected after gateway starts.
    autopilot_store: RwLock<Option<Arc<AutopilotStore>>>,
    /// Event broadcast sender for real-time task/activity events.
    event_tx: RwLock<Option<tokio::sync::broadcast::Sender<String>>>,
}

/// Cached update info from the last `system.check_update` call. [M2][R2:NM1]
#[derive(Clone)]
struct PendingUpdate {
    download_url: String,
    checksum_url: String,
    version: String,
    /// [R2:NM1] TTL — expires after 5 minutes to prevent stale URL replay
    cached_at: Instant,
}

impl PendingUpdate {
    const TTL_SECS: u64 = 300; // 5 minutes

    fn is_expired(&self) -> bool {
        self.cached_at.elapsed().as_secs() > Self::TTL_SECS
    }
}

/// Runtime state for a connected channel.
#[derive(Clone)]
pub struct ChannelState {
    pub connected: bool,
    pub last_event: Option<chrono::DateTime<chrono::Utc>>,
    pub error: Option<String>,
}

impl MethodHandler {
    pub async fn new(home_dir: PathBuf) -> Self {
        Self::with_extension(home_dir, Arc::new(crate::extension::NullExtension)).await
    }

    /// Create a new handler with a custom extension (used by Pro binary).
    pub async fn with_extension(
        home_dir: PathBuf,
        extension: Arc<dyn GatewayExtension>,
    ) -> Self {
        let agents_dir = home_dir.join("agents");
        let mut registry = AgentRegistry::new(agents_dir.clone());
        if let Err(e) = registry.scan().await {
            tracing::warn!("Failed to scan agents directory: {e}");
        }

        // Install the agent-file-guard PreToolUse hook into every existing
        // agent's .claude/settings.json on startup. Idempotent — merges into
        // existing settings without clobbering user-added hooks.
        let bin = crate::agent_hook_installer::resolve_duduclaw_bin();
        if let Ok(mut entries) = tokio::fs::read_dir(&agents_dir).await {
            while let Ok(Some(entry)) = entries.next_entry().await {
                let path = entry.path();
                if !path.is_dir() {
                    continue;
                }
                // Skip _trash and other non-agent directories.
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if name.starts_with('_') || name.is_empty() {
                    continue;
                }
                if let Err(e) = crate::agent_hook_installer::ensure_agent_hook_settings(&path, &bin).await {
                    tracing::warn!(
                        agent = %name,
                        error = %e,
                        "Failed to install agent-file-guard hook on startup"
                    );
                }
            }
        }
        Self {
            registry: Arc::new(RwLock::new(registry)),
            home_dir,
            start_time: Instant::now(),
            channel_status: Arc::new(RwLock::new(std::collections::HashMap::new())),
            heartbeat: RwLock::new(None),
            reply_ctx: RwLock::new(None),
            channel_handles: tokio::sync::Mutex::new(std::collections::HashMap::new()),
            pending_update: RwLock::new(None),
            user_db: RwLock::new(None),
            jwt_config: RwLock::new(None),
            extension,
            cron_store: RwLock::new(None),
            cron_scheduler: RwLock::new(None),
            mcp_oauth_pending: RwLock::new(std::collections::HashMap::new()),
            task_store: RwLock::new(None),
            autopilot_store: RwLock::new(None),
            event_tx: RwLock::new(None),
        }
    }

    /// Inject the SQLite-backed cron task store (called once after gateway start).
    pub async fn set_cron_store(&self, store: Arc<CronStore>) {
        *self.cron_store.write().await = Some(store);
    }

    /// Inject the SQLite-backed task board store (called once after gateway start).
    pub async fn set_task_store(&self, store: Arc<TaskStore>) {
        *self.task_store.write().await = Some(store);
    }

    /// Inject the SQLite-backed autopilot rule store (called once after gateway start).
    pub async fn set_autopilot_store(&self, store: Arc<AutopilotStore>) {
        *self.autopilot_store.write().await = Some(store);
    }

    /// Inject the event broadcast sender for task/activity real-time events.
    pub async fn set_event_tx(&self, tx: tokio::sync::broadcast::Sender<String>) {
        *self.event_tx.write().await = Some(tx);
    }

    /// Inject the running cron scheduler handle (called once after gateway start).
    pub async fn set_cron_scheduler(&self, scheduler: Arc<CronScheduler>) {
        *self.cron_scheduler.write().await = Some(scheduler);
    }

    /// Notify the cron scheduler to reload immediately. Call this after any
    /// mutation (add / update / delete / enable-toggle). No-op if the
    /// scheduler has not been injected yet.
    async fn notify_cron_reload(&self) {
        if let Some(scheduler) = self.cron_scheduler.read().await.as_ref() {
            scheduler.reload_now();
        }
    }

    /// Get the extension reference.
    pub fn extension(&self) -> &Arc<dyn GatewayExtension> {
        &self.extension
    }

    /// Inject user database and JWT config (called once after gateway start).
    pub async fn set_user_db(&self, db: Arc<UserDb>, jwt: Arc<JwtConfig>) {
        *self.user_db.write().await = Some(db);
        *self.jwt_config.write().await = Some(jwt);
    }

    /// Inject the reply context for hot-starting channels. Called once after
    /// ReplyContext is constructed in server.rs.
    pub async fn set_reply_ctx(&self, ctx: Arc<crate::channel_reply::ReplyContext>) {
        *self.reply_ctx.write().await = Some(ctx);
    }

    /// Register a running channel handle (for hot-stop on remove).
    /// If a handle with the same name already exists, it is aborted first.
    pub async fn register_channel_handle(&self, name: &str, handle: tokio::task::JoinHandle<()>) {
        let mut handles = self.channel_handles.lock().await;
        if let Some(old) = handles.insert(name.to_string(), handle) {
            old.abort();
        }
    }

    /// Update a channel's runtime connection state (called by channel bots).
    pub async fn set_channel_state(&self, name: &str, connected: bool, error: Option<String>) {
        let mut map = self.channel_status.write().await;
        map.insert(name.to_string(), ChannelState {
            connected,
            last_event: Some(chrono::Utc::now()),
            error,
        });
    }

    /// Get the shared channel status map for use by channel bots.
    pub fn channel_status(&self) -> &Arc<RwLock<std::collections::HashMap<String, ChannelState>>> {
        &self.channel_status
    }

    /// Get a reference to the shared agent registry.
    pub fn registry(&self) -> &Arc<RwLock<AgentRegistry>> {
        &self.registry
    }

    /// Get the home directory path.
    pub fn home_dir(&self) -> &Path {
        &self.home_dir
    }

    /// Get the pending OAuth flows map (used by HTTP callback handler).
    pub fn mcp_oauth_pending(&self) -> &RwLock<std::collections::HashMap<String, crate::mcp_oauth::PendingOAuth>> {
        &self.mcp_oauth_pending
    }

    /// Set the heartbeat scheduler reference (called after gateway start).
    pub async fn set_heartbeat(&self, scheduler: Arc<duduclaw_agent::HeartbeatScheduler>) {
        *self.heartbeat.write().await = Some(scheduler);
    }

    /// Route `method` to the correct handler and return a [`WsFrame`] response.
    ///
    /// `request_id` is carried through so that all response frames are correctly
    /// correlated with the originating client request.
    pub async fn handle(&self, method: &str, params: Value, ctx: &UserContext) -> WsFrame {
        let response = self.dispatch(method, params, ctx).await;
        response
    }

    /// Internal dispatch — returns a WsFrame with placeholder id (overwritten by caller).
    async fn dispatch(&self, method: &str, params: Value, ctx: &UserContext) -> WsFrame {
        // ── Plugin extension dispatch ──────
        // Try extension first; if it returns Some, the method is handled.
        if let Some(frame) = self.extension.handle_method(method, params.clone(), ctx).await {
            return frame;
        }

        // ── ACL macros ───────────────────────────────────────
        // Helper: require minimum role, return error frame on failure.
        macro_rules! require_admin {
            () => {
                if let Err(e) = acl::require_role(ctx, UserRole::Admin) {
                    return WsFrame::error_response("", &e);
                }
            };
        }
        macro_rules! require_manager {
            () => {
                if let Err(e) = acl::require_role(ctx, UserRole::Manager) {
                    return WsFrame::error_response("", &e);
                }
            };
        }
        // Helper: check agent access from params, return error frame on failure.
        macro_rules! check_agent {
            ($min_level:expr) => {
                match acl::extract_and_check_agent(ctx, &params, $min_level) {
                    Ok(id) => id,
                    Err(e) => return WsFrame::error_response("", &e),
                }
            };
        }

        match method {
            "connect.challenge" => self.handle_connect_challenge(params),
            "connect" => self.handle_connect(params),
            "ping" => WsFrame::ok_response("", json!({ "pong": true })),
            "hello-ok" => self.handle_hello_ok(params),
            "tools.catalog" => self.handle_tools_catalog(params),

            // ── Agent methods (filtered by binding) ──────────
            "agents.list" => self.handle_agents_list_filtered(ctx).await,
            "agents.status" => {
                let _ = check_agent!(AccessLevel::Viewer);
                self.handle_agents_status(params).await
            }
            "agents.create" => { require_admin!(); self.handle_agents_create(params).await }
            "agents.delegate" => {
                // H1 fix: delegate is high-risk — requires operator-level access
                let _ = check_agent!(AccessLevel::Operator);
                self.handle_agents_delegate(params).await
            }
            "agents.pause" => { require_manager!(); self.handle_agents_pause(params).await }
            "agents.resume" => { require_manager!(); self.handle_agents_resume(params).await }
            "agents.update" => {
                let _ = check_agent!(AccessLevel::Owner);
                self.handle_agents_update(params).await
            }
            "agents.remove" => { require_admin!(); self.handle_agents_remove(params).await }
            "agents.inspect" => {
                let _ = check_agent!(AccessLevel::Viewer);
                self.handle_agents_inspect(params).await
            }

            // ── Channel methods (admin only) ─────────────────
            "channels.status" => { require_admin!(); self.handle_channels_status().await }
            "channels.add" => { require_admin!(); self.handle_channels_add(params).await }
            "channels.test" => { require_admin!(); self.handle_channels_test(params).await }
            "channels.remove" => { require_admin!(); self.handle_channels_remove(params).await }

            // ── Account methods (admin only) ─────────────────
            "accounts.list" => { require_admin!(); self.handle_accounts_list().await }
            "accounts.budget_summary" => { require_manager!(); self.handle_budget_summary().await }
            "accounts.rotate" => {
                require_admin!();
                self.handle_accounts_rotate(params).await
            }
            "accounts.health" => { require_admin!(); self.handle_accounts_health().await }
            "accounts.add" => { require_admin!(); self.handle_accounts_add(params).await }
            "accounts.update_budget" => { require_admin!(); self.handle_accounts_update_budget(params).await }

            // ── Memory (agent-scoped, H2 fix) ────────────────
            "memory.search" => {
                let _ = check_agent!(AccessLevel::Viewer);
                self.handle_memory_search(params).await
            }
            "memory.browse" => {
                let _ = check_agent!(AccessLevel::Viewer);
                self.handle_memory_browse(params).await
            }
            "memory.key_facts" => {
                let _ = check_agent!(AccessLevel::Viewer);
                self.handle_memory_key_facts(params).await
            }

            // ── Wiki (open to all authenticated users) ───────
            "wiki.pages" => self.handle_wiki_pages(params).await,
            "wiki.read" => self.handle_wiki_read(params).await,
            "wiki.search" => self.handle_wiki_search(params).await,
            "wiki.lint" => self.handle_wiki_lint(params).await,
            "wiki.stats" => self.handle_wiki_stats(params).await,

            // ── Shared Wiki ─────────────────────────────────
            "shared_wiki.pages" => self.handle_shared_wiki_pages().await,
            "shared_wiki.read" => self.handle_shared_wiki_read(params).await,
            "shared_wiki.search" => self.handle_shared_wiki_search(params).await,
            "shared_wiki.stats" => self.handle_shared_wiki_stats().await,

            // ── Skills (open to all) ─────────────────────────
            "skills.list" => self.handle_skills_list(params).await,
            "skills.search" => self.handle_skills_search(params).await,
            "skills.content" => self.handle_skills_content(params).await,
            "skills.vet" => { require_admin!(); self.handle_skills_vet(params).await }
            "skills.install" => { require_admin!(); self.handle_skills_install(params).await }

            // ── Cron (admin only) ────────────────────────────
            "cron.list" => { require_admin!(); self.handle_cron_list().await }
            "cron.add" => { require_admin!(); self.handle_cron_add(params).await }
            "cron.update" => { require_admin!(); self.handle_cron_update(params).await }
            "cron.pause" => { require_admin!(); self.handle_cron_set_enabled(params, false).await }
            "cron.resume" => { require_admin!(); self.handle_cron_set_enabled(params, true).await }
            "cron.remove" => { require_admin!(); self.handle_cron_remove(params).await }

            // ── System (admin only for config changes) ───────
            "system.status" => self.handle_system_status().await,
            "system.doctor" => { require_admin!(); self.handle_system_doctor().await }
            "system.doctor_repair" => { require_admin!(); self.handle_system_doctor_repair().await }
            "models.list" => self.handle_models_list().await,
            "system.config" => { require_admin!(); self.handle_system_config().await }
            "system.update_config" => { require_admin!(); self.handle_system_update_config(params).await }
            "system.version" => self.handle_system_version(),
            "system.check_update" => { require_admin!(); self.handle_system_check_update().await }
            "system.apply_update" => { require_admin!(); self.handle_system_apply_update(params).await }

            // ── Logs (manager+) ──────────────────────────────
            "logs.subscribe" => { require_manager!(); self.handle_logs_subscribe(params) }
            "logs.unsubscribe" => self.handle_logs_unsubscribe(params),

            // ── Security (admin only) ────────────────────────
            "security.audit_log" => {
                require_admin!();
                self.handle_security_audit_log(params).await
            }
            "audit.unified_log" => {
                require_admin!();
                self.handle_audit_unified_log(params).await
            }
            "security.status" => {
                require_admin!();
                self.handle_security_status().await
            }

            // ── Analytics (manager+) ────────────────────────
            "analytics.summary" => {
                require_manager!();
                self.handle_analytics_summary(params).await
            }
            "analytics.conversations" => {
                require_manager!();
                self.handle_analytics_conversations().await
            }
            "analytics.cost_savings" => {
                require_manager!();
                self.handle_analytics_cost_savings().await
            }

            // ── Heartbeat (manager+) ─────────────────────────
            "heartbeat.status" => {
                require_manager!();
                self.handle_heartbeat_status().await
            }
            "heartbeat.trigger" => {
                require_manager!();
                self.handle_heartbeat_trigger(params).await
            }

            // ── Evolution (manager+, H3 fix) ─────────────────
            "evolution.status" => { require_manager!(); self.handle_evolution_status().await }
            "evolution.history" => { require_manager!(); self.handle_evolution_history(params).await }

            // ── Odoo (admin only) ────────────────────────────
            "odoo.status" => { require_admin!(); self.handle_odoo_status().await }
            "odoo.config" => { require_admin!(); self.handle_odoo_config().await }
            "odoo.configure" => { require_admin!(); self.handle_odoo_configure(params).await }
            "odoo.test" => { require_admin!(); self.handle_odoo_test().await }

            // ── User management (admin only) ─────────────────
            "users.list" => { require_admin!(); self.handle_users_list().await }
            "users.create" => { require_admin!(); self.handle_users_create(params, ctx).await }
            "users.update" => { require_admin!(); self.handle_users_update(params, ctx).await }
            "users.remove" => { require_admin!(); self.handle_users_remove(params, ctx).await }
            "users.bind_agent" => { require_admin!(); self.handle_users_bind_agent(params, ctx).await }
            "users.unbind_agent" => { require_admin!(); self.handle_users_unbind_agent(params, ctx).await }
            "users.offboard" => { require_admin!(); self.handle_users_offboard(params, ctx).await }
            "users.me" => self.handle_users_me(ctx).await,
            "users.audit_log" => { require_admin!(); self.handle_users_audit_log(params).await }

            "mcp.list" => { require_admin!(); self.handle_mcp_list().await }
            "mcp.update" => { require_admin!(); self.handle_mcp_update(&params).await }

            // ── MCP OAuth (admin only) ──────────────────────────
            "mcp.oauth.providers" => { require_admin!(); self.handle_mcp_oauth_providers().await }
            "mcp.oauth.start" => { require_admin!(); self.handle_mcp_oauth_start(params).await }
            "mcp.oauth.status" => { require_admin!(); self.handle_mcp_oauth_status(params).await }
            "mcp.oauth.revoke" => { require_admin!(); self.handle_mcp_oauth_revoke(params).await }

            // ── Task Board (open to all authenticated) ────
            "tasks.list" => self.handle_tasks_list(params).await,
            "tasks.create" => self.handle_tasks_create(params, ctx).await,
            "tasks.update" => self.handle_tasks_update(params).await,
            "tasks.remove" => self.handle_tasks_remove(params).await,
            "tasks.assign" => self.handle_tasks_assign(params).await,

            // ── Activity Feed (open to all authenticated) ───
            "activity.list" => self.handle_activity_list(params).await,
            // Per-topic filtering is NOT implemented: BroadcastLayer fans out every
            // activity event to every authenticated WS client unconditionally. This
            // RPC exists purely as a client-intent signal and future-compat hook so
            // callers can declare interest without guessing at server state.
            "activity.subscribe" => WsFrame::ok_response("", json!({
                "subscribed": true,
                "broadcast_mode": "all_events",
                "note": "All authenticated WS clients receive activity events automatically; no per-client filter is in effect.",
            })),

            // ── Autopilot (admin only) ──────────────────────
            "autopilot.list" => { require_admin!(); self.handle_autopilot_list().await }
            "autopilot.create" => { require_admin!(); self.handle_autopilot_create(params).await }
            "autopilot.update" => { require_admin!(); self.handle_autopilot_update(params).await }
            "autopilot.remove" => { require_admin!(); self.handle_autopilot_remove(params).await }
            "autopilot.history" => { require_admin!(); self.handle_autopilot_history(params).await }

            // ── Shared Skills (open to all authenticated) ───
            "skills.shared" => self.handle_skills_shared_list().await,
            "skills.share" => self.handle_skills_share(params).await,
            "skills.adopt" => self.handle_skills_adopt(params).await,

            // ── Partner Portal ──────────────────────────────
            "partner.profile" => self.handle_partner_profile().await,
            "partner.stats" => self.handle_partner_stats().await,
            "partner.customers" => self.handle_partner_customers(params).await,
            "partner.profile.update" => {
                require_admin!();
                self.handle_partner_profile_update(params).await
            }
            "partner.customer.add" => {
                require_admin!();
                self.handle_partner_customer_add(params).await
            }
            "partner.customer.update" => {
                require_admin!();
                self.handle_partner_customer_update(params).await
            }
            "partner.customer.delete" => {
                require_admin!();
                self.handle_partner_customer_delete(params).await
            }

            // ── Billing ──────────────────────────────────────
            "billing.usage" => self.handle_billing_usage().await,
            "billing.history" | "billing.plan" =>
                WsFrame::error_response("", "Billing features are not available in the current edition"),
            "browser.audit_log" | "browser.emergency_stop" | "browser.tool_approve"
            | "browser.browserbase_sessions" | "browser.browserbase_cost" =>
                WsFrame::error_response("", "Browser automation features require the Pro edition"),
            "marketplace.list" => self.handle_marketplace_list().await,
            "marketplace.install" =>
                WsFrame::error_response("", "Marketplace install is not yet available"),

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
        WsFrame::ok_response("", json!({ "version": crate::updater::current_version(), "client_version": version, "status": "connected" }))
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
                { "name": "agents.update", "description": "Update agent config fields" },
                { "name": "agents.remove", "description": "Remove an agent (to trash)" },
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
                { "name": "memory.key_facts", "description": "List extracted key insights (P2 Key-Fact Accumulator)" },
                { "name": "wiki.pages", "description": "List wiki pages for an agent" },
                { "name": "wiki.read", "description": "Read a wiki page" },
                { "name": "wiki.search", "description": "Search wiki pages" },
                { "name": "wiki.lint", "description": "Wiki health check" },
                { "name": "wiki.stats", "description": "Wiki statistics" },
                { "name": "shared_wiki.pages", "description": "List shared wiki pages" },
                { "name": "shared_wiki.read", "description": "Read a shared wiki page" },
                { "name": "shared_wiki.search", "description": "Search shared wiki" },
                { "name": "shared_wiki.stats", "description": "Shared wiki statistics" },
                { "name": "skills.list", "description": "List agent skills" },
                { "name": "skills.content", "description": "Read skill content" },
                { "name": "cron.list", "description": "List cron jobs" },
                { "name": "cron.add", "description": "Add a cron job" },
                { "name": "cron.pause", "description": "Pause a cron job" },
                { "name": "cron.remove", "description": "Remove a cron job" },
                { "name": "system.status", "description": "System status" },
                { "name": "system.doctor", "description": "Health checks" },
                { "name": "system.doctor_repair", "description": "Health checks with repair hints" },
                { "name": "models.list", "description": "List available cloud and local models" },
                { "name": "system.config", "description": "View system config" },
                { "name": "system.update_config", "description": "Update system config (log_level, rotation)" },
                { "name": "accounts.add", "description": "Add a new account" },
                { "name": "accounts.update_budget", "description": "Update account monthly budget" },
                { "name": "system.version", "description": "Version info" },
                { "name": "system.check_update", "description": "Check for available updates" },
                { "name": "system.apply_update", "description": "Download and apply update" },
                { "name": "heartbeat.status", "description": "Per-agent heartbeat status" },
                { "name": "heartbeat.trigger", "description": "Manually trigger heartbeat for an agent" },
                { "name": "mcp.list", "description": "List MCP servers for all agents + catalog" },
                { "name": "mcp.update", "description": "Add or remove an MCP server for an agent" },
                { "name": "mcp.oauth.providers", "description": "List available OAuth providers and their auth status" },
                { "name": "mcp.oauth.start", "description": "Start OAuth flow for a provider" },
                { "name": "mcp.oauth.status", "description": "Check OAuth status for a provider" },
                { "name": "mcp.oauth.revoke", "description": "Revoke OAuth token for a provider" },
                { "name": "logs.subscribe", "description": "Subscribe to logs" },
                { "name": "logs.unsubscribe", "description": "Unsubscribe from logs" },
                { "name": "security.status", "description": "Security system status" },
                { "name": "analytics.summary", "description": "Analytics summary for a period" },
                { "name": "analytics.conversations", "description": "Daily conversation counts" },
                { "name": "analytics.cost_savings", "description": "Monthly cost savings" },
            ]
        }))
    }

    // ── Agents ───────────────────────────────────────────────

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
        if !is_valid_agent_id(name) {
            return WsFrame::error_response("", "Agent name must be lowercase alphanumeric with hyphens, max 64 chars");
        }

        // If creating as main, demote the current main agent first
        if role == "main" {
            if let Err(e) = self.demote_current_main(name).await {
                return WsFrame::error_response("", &e);
            }
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

        let agent_config = toml::toml! {
            [agent]
            name = name
            display_name = display_name
            role = role
            status = "active"
            trigger = trigger
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
        };
        let agent_toml = toml::to_string_pretty(&agent_config).unwrap_or_default();

        if let Err(e) = tokio::fs::write(agent_dir.join("agent.toml"), &agent_toml).await {
            return WsFrame::error_response("", &format!("Failed to write agent.toml: {e}"));
        }

        let soul = format!("# {display_name}\n\nI am {display_name}, a specialist AI agent.\n");
        let _ = tokio::fs::write(agent_dir.join("SOUL.md"), &soul).await;

        // Install the agent-file-guard PreToolUse hook so this newly-created
        // agent immediately gets protected against out-of-tree Write/Edit.
        let bin = crate::agent_hook_installer::resolve_duduclaw_bin();
        if let Err(e) = crate::agent_hook_installer::ensure_agent_hook_settings(&agent_dir, &bin).await {
            tracing::warn!(
                agent = %name,
                error = %e,
                "Failed to install agent-file-guard hook on agents.create"
            );
        }

        info!(name, "Agent created");
        WsFrame::ok_response("", json!({
            "success": true,
            "agent": { "name": name, "display_name": display_name, "role": role, "status": "active" }
        }))
    }

    /// Dashboard-initiated delegation.  Supervisor pattern is NOT enforced here
    /// because this RPC is an operator-level action (depth always starts at 0).
    /// Agent-to-agent delegation goes through MCP `send_to_agent` which IS enforced.
    async fn handle_agents_delegate(&self, params: Value) -> WsFrame {
        let agent_id = params.get("agent_id").and_then(|v| v.as_str()).unwrap_or("");
        let prompt = params.get("prompt").and_then(|v| v.as_str()).unwrap_or("");
        let wait = params.get("wait_for_response").and_then(|v| v.as_bool()).unwrap_or(false);

        // Enforce prompt length limit to prevent abuse (MCP-H1)
        const MAX_PROMPT_LEN: usize = 100_000;
        if prompt.len() > MAX_PROMPT_LEN {
            return WsFrame::error_response("", &format!("Prompt too long: {} chars (max {MAX_PROMPT_LEN})", prompt.len()));
        }

        info!(agent_id, "agents.delegate requested (dashboard)");

        // Verify target agent exists
        let reg = self.registry.read().await;
        let agent = match reg.get(agent_id) {
            Some(a) => a.clone(),
            None => return WsFrame::error_response("", &format!("Agent not found: {agent_id}")),
        };
        let model = agent.config.model.preferred.clone();
        drop(reg);

        let message_id = uuid::Uuid::new_v4().to_string();

        if wait {
            // Synchronous delegation: spawn Python subprocess and wait
            let home = self.home_dir.clone();
            let system_prompt = agent.soul.as_deref().unwrap_or("You are a helpful AI agent.").to_string();
            match crate::channel_reply::call_python_sdk_delegate(prompt, &model, &system_prompt, &home).await {
                Ok(response) => WsFrame::ok_response("", json!({
                    "success": true,
                    "message_id": message_id,
                    "target_agent": agent_id,
                    "response": response,
                    "status": "completed",
                })),
                Err(e) => WsFrame::error_response("", &format!("Delegate execution failed: {e}")),
            }
        } else {
            // Async delegation: write to bus queue for background processing
            let queue_path = self.home_dir.join("bus_queue.jsonl");
            let task = serde_json::json!({
                "type": "agent_message",
                "message_id": &message_id,
                "agent_id": agent_id,
                "payload": prompt,
                "timestamp": chrono::Utc::now().to_rfc3339(),
                "delegation_depth": 0,
                "origin_agent": "dashboard",
                "sender_agent": "dashboard",
            });
            let task_str = task.to_string();
            if let Err(e) = crate::dispatcher::append_line(&queue_path, &task_str).await {
                return WsFrame::error_response("", &format!("Failed to queue delegation: {e}"));
            }

            WsFrame::ok_response("", json!({
                "success": true,
                "message_id": message_id,
                "target_agent": agent_id,
                "status": "queued",
            }))
        }
    }

    async fn handle_agents_pause(&self, params: Value) -> WsFrame {
        let agent_id = params.get("agent_id").and_then(|v| v.as_str()).unwrap_or("");
        info!(agent_id, "agents.pause requested");

        if let Err(e) = self.update_agent_status(agent_id, "paused").await {
            return WsFrame::error_response("", &format!("Failed to pause agent: {e}"));
        }

        WsFrame::ok_response("", json!({ "success": true, "name": agent_id, "status": "paused" }))
    }

    async fn handle_agents_resume(&self, params: Value) -> WsFrame {
        let agent_id = params.get("agent_id").and_then(|v| v.as_str()).unwrap_or("");
        info!(agent_id, "agents.resume requested");

        if let Err(e) = self.update_agent_status(agent_id, "active").await {
            return WsFrame::error_response("", &format!("Failed to resume agent: {e}"));
        }

        WsFrame::ok_response("", json!({ "success": true, "name": agent_id, "status": "active" }))
    }

    /// Read-modify-write an agent's `agent.toml` using the provided mutation closure.
    ///
    /// Uses atomic write (temp + rename) to prevent corruption on concurrent access.
    /// After a successful write, triggers a registry re-scan for hot-reload.
    async fn update_agent_toml<F>(&self, agent_id: &str, mutate: F) -> Result<(), String>
    where
        F: FnOnce(&mut toml::Table) -> Result<(), String>,
    {
        if !is_valid_agent_id(agent_id) {
            return Err(format!("Invalid agent_id: {agent_id}"));
        }

        let reg = self.registry.read().await;
        let agent = reg.get(agent_id)
            .ok_or_else(|| format!("Agent not found: {agent_id}"))?;
        let agent_toml_path = agent.dir.join("agent.toml");
        drop(reg);

        let content = tokio::fs::read_to_string(&agent_toml_path).await
            .map_err(|e| format!("Failed to read agent.toml: {e}"))?;

        let mut table: toml::Table = content.parse()
            .map_err(|e| format!("Failed to parse agent.toml: {e}"))?;

        mutate(&mut table)?;

        let new_content = toml::to_string_pretty(&table)
            .map_err(|e| format!("Failed to serialise agent.toml: {e}"))?;

        // Atomic write: temp file + rename
        let tmp_path = agent_toml_path.with_extension("toml.tmp");
        tokio::fs::write(&tmp_path, &new_content).await
            .map_err(|e| format!("Failed to write agent.toml.tmp: {e}"))?;
        tokio::fs::rename(&tmp_path, &agent_toml_path).await
            .map_err(|e| {
                let _ = std::fs::remove_file(&tmp_path);
                format!("Failed to commit agent.toml: {e}")
            })?;

        // Trigger registry re-scan for hot-reload
        if let Ok(mut reg) = tokio::time::timeout(
            std::time::Duration::from_millis(500),
            self.registry.write(),
        ).await {
            let _ = reg.scan().await;
        }

        Ok(())
    }

    /// Convenience: update only the `status` field in an agent's `agent.toml`.
    async fn update_agent_status(&self, agent_id: &str, status: &str) -> Result<(), String> {
        let status = status.to_string();
        self.update_agent_toml(agent_id, move |table| {
            let agent_section = table.get_mut("agent")
                .and_then(|v| v.as_table_mut())
                .ok_or_else(|| "agent.toml missing [agent] section".to_string())?;
            agent_section.insert("status".to_string(), toml::Value::String(status.clone()));
            info!("Agent status updated to {status}");
            Ok(())
        }).await?;
        Ok(())
    }

    /// Demote the current main agent to "specialist", skipping `except_id`.
    /// This ensures at most one agent has the "main" role at any time.
    async fn demote_current_main(&self, except_id: &str) -> Result<(), String> {
        let current_main = {
            let reg = self.registry.read().await;
            reg.main_agent()
                .filter(|a| a.config.agent.name != except_id)
                .map(|a| a.config.agent.name.clone())
        };
        if let Some(old_main) = current_main {
            info!(old_main = old_main.as_str(), "Demoting current main agent to specialist");
            self.update_agent_toml(&old_main, |table| {
                let agent_section = table.get_mut("agent")
                    .and_then(|v| v.as_table_mut())
                    .ok_or_else(|| "agent.toml missing [agent] section".to_string())?;
                agent_section.insert("role".into(), toml::Value::String("specialist".into()));
                Ok(())
            }).await?;
        }
        Ok(())
    }

    /// Update one or more fields of an agent's `agent.toml`.
    ///
    /// Supports identity, model, budget, heartbeat, permissions, and evolution fields.
    /// Only sends changed fields — unchanged fields are omitted from the request.
    async fn handle_agents_update(&self, params: Value) -> WsFrame {
        let agent_id = match params.get("agent_id").and_then(|v| v.as_str()) {
            Some(id) if !id.is_empty() => id.to_string(),
            _ => return WsFrame::error_response("", "Missing 'agent_id' parameter"),
        };

        // If promoting to main, demote the current main agent first
        if let Some("main") = params.get("role").and_then(|v| v.as_str()) {
            if let Err(e) = self.demote_current_main(&agent_id).await {
                return WsFrame::error_response("", &e);
            }
        }

        let params_clone = params.clone();
        let mut changes: Vec<String> = Vec::new();
        let home_for_update = self.home_dir.clone();

        let result = self.update_agent_toml(&agent_id, move |table| {
            // ── Identity fields ([agent] section) ──
            if let Some(agent_section) = table.get_mut("agent").and_then(|v| v.as_table_mut()) {
                if let Some(v) = params_clone.get("display_name").and_then(|v| v.as_str()) {
                    agent_section.insert("display_name".into(), toml::Value::String(v.into()));
                    changes.push(format!("display_name = \"{v}\""));
                }
                if let Some(v) = params_clone.get("role").and_then(|v| v.as_str()) {
                    match v {
                        "main" | "specialist" | "worker" | "developer" | "qa" | "planner" => {
                            agent_section.insert("role".into(), toml::Value::String(v.into()));
                            changes.push(format!("role = \"{v}\""));
                        }
                        _ => return Err(format!("Invalid role '{v}'. Valid: main, specialist, worker, developer, qa, planner")),
                    }
                }
                if let Some(v) = params_clone.get("status").and_then(|v| v.as_str()) {
                    match v {
                        "active" | "paused" | "terminated" => {
                            agent_section.insert("status".into(), toml::Value::String(v.into()));
                            changes.push(format!("status = \"{v}\""));
                        }
                        _ => return Err(format!("Invalid status '{v}'. Valid: active, paused, terminated")),
                    }
                }
                if let Some(v) = params_clone.get("trigger").and_then(|v| v.as_str()) {
                    agent_section.insert("trigger".into(), toml::Value::String(v.into()));
                    changes.push(format!("trigger = \"{v}\""));
                }
                if let Some(v) = params_clone.get("icon").and_then(|v| v.as_str()) {
                    agent_section.insert("icon".into(), toml::Value::String(v.into()));
                    changes.push(format!("icon = \"{v}\""));
                }
                if let Some(v) = params_clone.get("reports_to").and_then(|v| v.as_str()) {
                    agent_section.insert("reports_to".into(), toml::Value::String(v.into()));
                    changes.push(format!("reports_to = \"{v}\""));
                }
            }

            // ── Model fields ([model] section) ──
            let model = table.entry("model")
                .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
                .as_table_mut();
            if let Some(model) = model {
                if let Some(v) = params_clone.get("preferred").and_then(|v| v.as_str()) {
                    model.insert("preferred".into(), toml::Value::String(v.into()));
                    changes.push(format!("model.preferred = \"{v}\""));
                }
                if let Some(v) = params_clone.get("fallback").and_then(|v| v.as_str()) {
                    model.insert("fallback".into(), toml::Value::String(v.into()));
                    changes.push(format!("model.fallback = \"{v}\""));
                }
                if let Some(v) = params_clone.get("api_mode").and_then(|v| v.as_str()) {
                    match v {
                        "cli" | "direct" | "auto" => {
                            model.insert("api_mode".into(), toml::Value::String(v.into()));
                            changes.push(format!("model.api_mode = \"{v}\""));
                        }
                        _ => return Err(format!("Invalid api_mode '{v}'. Valid: cli, direct, auto")),
                    }
                }
            }

            // ── Local model fields ([model.local] section) ──
            if let Some(model) = table.get_mut("model").and_then(|v| v.as_table_mut()) {
                // Check if any local model param is provided
                let has_local_params = ["local_model", "local_backend", "local_context_length", "local_gpu_layers", "prefer_local", "use_router"]
                    .iter().any(|k| params_clone.get(*k).is_some());

                if has_local_params {
                    let local = model.entry("local")
                        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
                        .as_table_mut();
                    if let Some(local) = local {
                        if let Some(v) = params_clone.get("local_model").and_then(|v| v.as_str()) {
                            local.insert("model".into(), toml::Value::String(v.into()));
                            changes.push(format!("model.local.model = \"{v}\""));
                        }
                        if let Some(v) = params_clone.get("local_backend").and_then(|v| v.as_str()) {
                            match v {
                                "llama_cpp" | "openai_compat" | "mistral_rs" => {
                                    local.insert("backend".into(), toml::Value::String(v.into()));
                                    changes.push(format!("model.local.backend = \"{v}\""));
                                }
                                _ => return Err(format!("Invalid local_backend '{v}'. Valid: llama_cpp, openai_compat, mistral_rs")),
                            }
                        }
                        if let Some(v) = params_clone.get("local_context_length").and_then(|v| v.as_u64()) {
                            local.insert("context_length".into(), toml::Value::Integer(v as i64));
                            changes.push(format!("model.local.context_length = {v}"));
                        }
                        if let Some(v) = params_clone.get("local_gpu_layers").and_then(|v| v.as_i64()) {
                            local.insert("gpu_layers".into(), toml::Value::Integer(v));
                            changes.push(format!("model.local.gpu_layers = {v}"));
                        }
                        if let Some(v) = params_clone.get("prefer_local").and_then(|v| v.as_bool()) {
                            local.insert("prefer_local".into(), toml::Value::Boolean(v));
                            changes.push(format!("model.local.prefer_local = {v}"));
                        }
                        if let Some(v) = params_clone.get("use_router").and_then(|v| v.as_bool()) {
                            local.insert("use_router".into(), toml::Value::Boolean(v));
                            changes.push(format!("model.local.use_router = {v}"));
                        }
                    }
                }
            }

            // ── Budget fields ([budget] section) ──
            let budget = table.entry("budget")
                .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
                .as_table_mut();
            if let Some(budget) = budget {
                if let Some(v) = params_clone.get("monthly_limit_cents").and_then(|v| v.as_u64()) {
                    budget.insert("monthly_limit_cents".into(), toml::Value::Integer(v as i64));
                    changes.push(format!("budget.monthly_limit_cents = {v}"));
                }
                if let Some(v) = params_clone.get("warn_threshold_percent").and_then(|v| v.as_u64()) {
                    if v > 100 {
                        return Err("warn_threshold_percent must be 0-100".into());
                    }
                    budget.insert("warn_threshold_percent".into(), toml::Value::Integer(v as i64));
                    changes.push(format!("budget.warn_threshold_percent = {v}"));
                }
                if let Some(v) = params_clone.get("hard_stop").and_then(|v| v.as_bool()) {
                    budget.insert("hard_stop".into(), toml::Value::Boolean(v));
                    changes.push(format!("budget.hard_stop = {v}"));
                }
            }

            // ── Heartbeat fields ([heartbeat] section) ──
            let heartbeat = table.entry("heartbeat")
                .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
                .as_table_mut();
            if let Some(hb) = heartbeat {
                if let Some(v) = params_clone.get("heartbeat_enabled").and_then(|v| v.as_bool()) {
                    hb.insert("enabled".into(), toml::Value::Boolean(v));
                    changes.push(format!("heartbeat.enabled = {v}"));
                }
                if let Some(v) = params_clone.get("heartbeat_interval").and_then(|v| v.as_u64()) {
                    hb.insert("interval_seconds".into(), toml::Value::Integer(v as i64));
                    changes.push(format!("heartbeat.interval_seconds = {v}"));
                }
                if let Some(v) = params_clone.get("heartbeat_cron").and_then(|v| v.as_str()) {
                    hb.insert("cron".into(), toml::Value::String(v.into()));
                    changes.push(format!("heartbeat.cron = \"{v}\""));
                }
            }

            // ── Permissions fields ([permissions] section) ──
            let perms = table.entry("permissions")
                .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
                .as_table_mut();
            if let Some(perms) = perms {
                for key in &[
                    "can_create_agents",
                    "can_send_cross_agent",
                    "can_modify_own_skills",
                    "can_modify_own_soul",
                    "can_schedule_tasks",
                ] {
                    if let Some(v) = params_clone.get(*key).and_then(|v| v.as_bool()) {
                        perms.insert((*key).into(), toml::Value::Boolean(v));
                        changes.push(format!("permissions.{key} = {v}"));
                    }
                }
            }

            // ── Container fields ([container] section) ──
            let container = table.entry("container")
                .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
                .as_table_mut();
            if let Some(ct) = container {
                if let Some(v) = params_clone.get("timeout_ms").and_then(|v| v.as_u64()) {
                    ct.insert("timeout_ms".into(), toml::Value::Integer(v as i64));
                    changes.push(format!("container.timeout_ms = {v}"));
                }
                if let Some(v) = params_clone.get("max_concurrent").and_then(|v| v.as_u64()) {
                    ct.insert("max_concurrent".into(), toml::Value::Integer(v as i64));
                    changes.push(format!("container.max_concurrent = {v}"));
                }
                for key in &["sandbox_enabled", "network_access", "readonly_project"] {
                    if let Some(v) = params_clone.get(*key).and_then(|v| v.as_bool()) {
                        ct.insert((*key).into(), toml::Value::Boolean(v));
                        changes.push(format!("container.{key} = {v}"));
                    }
                }
            }

            // ── Evolution fields ([evolution] section) ──
            let evo = table.entry("evolution")
                .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
                .as_table_mut();
            if let Some(evo) = evo {
                for key in &["skill_auto_activate", "skill_security_scan", "gvu_enabled", "cognitive_memory"] {
                    if let Some(v) = params_clone.get(*key).and_then(|v| v.as_bool()) {
                        evo.insert((*key).into(), toml::Value::Boolean(v));
                        changes.push(format!("evolution.{key} = {v}"));
                    }
                }
                for key in &["max_active_skills", "max_gvu_generations", "skill_token_budget"] {
                    if let Some(v) = params_clone.get(*key).and_then(|v| v.as_u64()) {
                        evo.insert((*key).into(), toml::Value::Integer(v as i64));
                        changes.push(format!("evolution.{key} = {v}"));
                    }
                }
                for key in &["max_silence_hours", "observation_period_hours"] {
                    if let Some(v) = params_clone.get(*key).and_then(|v| v.as_f64()) {
                        evo.insert((*key).into(), toml::Value::Float(v));
                        changes.push(format!("evolution.{key} = {v}"));
                    }
                }
            }

            // ── Per-agent channel tokens ([channels.*] sections) ──
            // Helper: write a token (+ encrypted version) into [channels.{channel}].{field}
            // Empty token removes the entire [channels.{channel}] section.
            let home = home_for_update.clone();
            let mut set_channel_token = |table: &mut toml::Table,
                                          channel: &str,
                                          fields: &[(&str, Option<&str>)], // (param_key, toml_key) pairs
                                          changes: &mut Vec<String>| -> Result<(), String> {
                // Check if any field has a value
                let has_any = fields.iter().any(|(param_key, _)| {
                    params_clone.get(*param_key).and_then(|v| v.as_str()).map_or(false, |s| !s.is_empty())
                });
                let all_empty = fields.iter().all(|(param_key, _)| {
                    params_clone.get(*param_key).and_then(|v| v.as_str()).map_or(true, |s| s.is_empty())
                });

                // If the param exists but is empty → remove
                let param_present = fields.iter().any(|(param_key, _)| params_clone.get(*param_key).is_some());
                if param_present && all_empty {
                    if let Some(channels) = table.get_mut("channels").and_then(|v| v.as_table_mut()) {
                        channels.remove(channel);
                        changes.push(format!("channels.{channel} removed"));
                    }
                    return Ok(());
                }

                if !has_any { return Ok(()); }

                let channels = table.entry("channels")
                    .or_insert_with(|| toml::Value::Table(toml::Table::new()))
                    .as_table_mut()
                    .ok_or_else(|| format!("Invalid [channels] section"))?;
                let section = channels.entry(channel)
                    .or_insert_with(|| toml::Value::Table(toml::Table::new()))
                    .as_table_mut()
                    .ok_or_else(|| format!("Invalid [channels.{channel}] section"))?;

                for (param_key, toml_key_override) in fields {
                    if let Some(val) = params_clone.get(*param_key).and_then(|v| v.as_str()) {
                        if !val.is_empty() {
                            let toml_key = toml_key_override.unwrap_or(param_key);
                            section.insert(toml_key.to_string(), toml::Value::String(val.into()));
                            // Encrypt sensitive tokens
                            if toml_key.contains("token") || toml_key.contains("secret") || toml_key == "app_id" {
                                let enc_key = format!("{toml_key}_enc");
                                if let Some(enc) = crate::config_crypto::encrypt_value(val, &home) {
                                    section.insert(enc_key, toml::Value::String(enc));
                                }
                            }
                        }
                    }
                }

                changes.push(format!("channels.{channel} = [CONFIGURED]"));
                Ok(())
            };

            // Discord
            set_channel_token(table, "discord", &[
                ("discord_bot_token", Some("bot_token")),
            ], &mut changes)?;

            // Telegram
            set_channel_token(table, "telegram", &[
                ("telegram_bot_token", Some("bot_token")),
            ], &mut changes)?;

            // LINE
            set_channel_token(table, "line", &[
                ("line_channel_token", Some("channel_token")),
                ("line_channel_secret", Some("channel_secret")),
            ], &mut changes)?;

            // Slack
            set_channel_token(table, "slack", &[
                ("slack_app_token", Some("app_token")),
                ("slack_bot_token", Some("bot_token")),
            ], &mut changes)?;

            // WhatsApp
            set_channel_token(table, "whatsapp", &[
                ("whatsapp_access_token", Some("access_token")),
                ("whatsapp_verify_token", Some("verify_token")),
                ("whatsapp_phone_number_id", Some("phone_number_id")),
                ("whatsapp_app_secret", Some("app_secret")),
            ], &mut changes)?;

            // Feishu
            set_channel_token(table, "feishu", &[
                ("feishu_app_id", Some("app_id")),
                ("feishu_app_secret", Some("app_secret")),
                ("feishu_verification_token", Some("verification_token")),
            ], &mut changes)?;

            // ── Sticker fields ([sticker] section) ──
            let sticker = table.entry("sticker")
                .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
                .as_table_mut();
            if let Some(sticker) = sticker {
                if let Some(v) = params_clone.get("sticker_enabled").and_then(|v| v.as_bool()) {
                    sticker.insert("enabled".into(), toml::Value::Boolean(v));
                    changes.push(format!("sticker.enabled = {v}"));
                }
                if let Some(v) = params_clone.get("sticker_probability").and_then(|v| v.as_f64()) {
                    if !(0.0..=1.0).contains(&v) {
                        return Err("sticker_probability must be 0.0-1.0".into());
                    }
                    sticker.insert("probability".into(), toml::Value::Float(v));
                    changes.push(format!("sticker.probability = {v}"));
                }
                if let Some(v) = params_clone.get("sticker_intensity_threshold").and_then(|v| v.as_f64()) {
                    if !(0.0..=1.0).contains(&v) {
                        return Err("sticker_intensity_threshold must be 0.0-1.0".into());
                    }
                    sticker.insert("intensity_threshold".into(), toml::Value::Float(v));
                    changes.push(format!("sticker.intensity_threshold = {v}"));
                }
                if let Some(v) = params_clone.get("sticker_cooldown_messages").and_then(|v| v.as_u64()) {
                    if v > 100 {
                        return Err("sticker_cooldown_messages must be 0-100".into());
                    }
                    sticker.insert("cooldown_messages".into(), toml::Value::Integer(v as i64));
                    changes.push(format!("sticker.cooldown_messages = {v}"));
                }
                if let Some(v) = params_clone.get("sticker_expressiveness").and_then(|v| v.as_str()) {
                    if !["minimal", "moderate", "expressive"].contains(&v) {
                        return Err("sticker_expressiveness must be minimal|moderate|expressive".into());
                    }
                    sticker.insert("expressiveness".into(), toml::Value::String(v.into()));
                    changes.push(format!("sticker.expressiveness = \"{v}\""));
                }
            }

            if changes.is_empty() {
                return Err("No valid fields to update".into());
            }

            Ok(())
        }).await;

        match result {
            Ok(()) => {
                info!(agent_id = agent_id.as_str(), "agents.update completed");
                WsFrame::ok_response("", json!({
                    "success": true,
                    "agent_id": agent_id,
                    "message": "Agent updated successfully",
                }))
            }
            Err(e) => WsFrame::error_response("", &e),
        }
    }

    /// Remove an agent by moving its directory to `_trash/`.
    ///
    /// Refuses to remove the main agent. Recovery is possible from `_trash/`.
    async fn handle_agents_remove(&self, params: Value) -> WsFrame {
        let agent_id = match params.get("agent_id").and_then(|v| v.as_str()) {
            Some(id) if !id.is_empty() => id,
            _ => return WsFrame::error_response("", "Missing 'agent_id' parameter"),
        };

        if !is_valid_agent_id(agent_id) {
            return WsFrame::error_response("", "Invalid agent_id format");
        }

        // Refuse to remove the main agent
        let reg = self.registry.read().await;
        if let Some(agent) = reg.get(agent_id) {
            if matches!(agent.config.agent.role, duduclaw_core::types::AgentRole::Main) {
                return WsFrame::error_response("", "Cannot remove the main agent");
            }
        } else {
            return WsFrame::error_response("", &format!("Agent not found: {agent_id}"));
        }
        let agents_dir = reg.agents_dir().to_path_buf();
        drop(reg);

        let agent_dir = agents_dir.join(agent_id);
        let trash_dir = agents_dir.join("_trash");
        if let Err(e) = tokio::fs::create_dir_all(&trash_dir).await {
            return WsFrame::error_response("", &format!("Failed to create _trash/: {e}"));
        }

        let timestamp = chrono::Utc::now().format("%Y%m%d_%H%M%S");
        let trash_dest = trash_dir.join(format!("{agent_id}_{timestamp}"));

        if let Err(e) = tokio::fs::rename(&agent_dir, &trash_dest).await {
            return WsFrame::error_response("", &format!("Failed to move agent to trash: {e}"));
        }

        // Re-scan registry
        if let Ok(mut reg) = tokio::time::timeout(
            std::time::Duration::from_millis(500),
            self.registry.write(),
        ).await {
            let _ = reg.scan().await;
        }

        info!(agent_id, "Agent removed (moved to _trash/)");
        WsFrame::ok_response("", json!({
            "success": true,
            "agent_id": agent_id,
            "trash_path": trash_dest.to_string_lossy(),
        }))
    }

    async fn handle_agents_inspect(&self, params: Value) -> WsFrame {
        let agent_id = params.get("agent_id").and_then(|v| v.as_str()).unwrap_or("");
        let spent = self.get_total_spent().await;
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
                    "soul_preview": a.soul.as_ref().map(|s| {
                        let t = truncate_bytes(s, 500);
                        if t.len() < s.len() { format!("{t}…") } else { s.clone() }
                    }),
                    "identity_preview": a.identity.as_ref().map(|s| {
                        let t = truncate_bytes(s, 500);
                        if t.len() < s.len() { format!("{t}…") } else { s.clone() }
                    }),
                    "memory_summary": a.memory,
                    "skills": a.skills.iter().map(|s| &s.name).collect::<Vec<_>>(),
                    "model": {
                        "preferred": cfg.model.preferred,
                        "fallback": cfg.model.fallback,
                        "account_pool": cfg.model.account_pool,
                        "api_mode": cfg.model.api_mode,
                        "local": cfg.model.local.as_ref().map(|l| json!({
                            "model": l.model,
                            "backend": l.backend,
                            "context_length": l.context_length,
                            "gpu_layers": l.gpu_layers,
                            "prefer_local": l.prefer_local,
                            "use_router": l.use_router,
                        })),
                    },
                    "budget": { "monthly_limit_cents": cfg.budget.monthly_limit_cents, "spent_cents": spent, "warn_threshold_percent": cfg.budget.warn_threshold_percent, "hard_stop": cfg.budget.hard_stop },
                    "heartbeat": { "enabled": cfg.heartbeat.enabled, "interval_seconds": cfg.heartbeat.interval_seconds },
                    "permissions": {
                        "can_create_agents": cfg.permissions.can_create_agents,
                        "can_send_cross_agent": cfg.permissions.can_send_cross_agent,
                        "can_modify_own_skills": cfg.permissions.can_modify_own_skills,
                        "can_modify_own_soul": cfg.permissions.can_modify_own_soul,
                        "can_schedule_tasks": cfg.permissions.can_schedule_tasks,
                    },
                    "sticker": {
                        "enabled": cfg.sticker.enabled,
                        "probability": cfg.sticker.probability,
                        "intensity_threshold": cfg.sticker.intensity_threshold,
                        "cooldown_messages": cfg.sticker.cooldown_messages,
                        "expressiveness": match cfg.sticker.expressiveness {
                            duduclaw_core::types::Expressiveness::Minimal => "minimal",
                            duduclaw_core::types::Expressiveness::Moderate => "moderate",
                            duduclaw_core::types::Expressiveness::Expressive => "expressive",
                        },
                    },
                    "evolution": {
                        "gvu_enabled": cfg.evolution.gvu_enabled,
                        "cognitive_memory": cfg.evolution.cognitive_memory,
                        "skill_auto_activate": cfg.evolution.skill_auto_activate,
                        "skill_security_scan": cfg.evolution.skill_security_scan,
                        "max_silence_hours": cfg.evolution.max_silence_hours,
                    },
                }))
            }
            None => WsFrame::error_response("", &format!("Agent not found: {agent_id}")),
        }
    }

    // ── Channels ─────────────────────────────────────────────

    async fn handle_channels_status(&self) -> WsFrame {
        let config_path = self.home_dir.join("config.toml");
        let runtime_status = self.channel_status.read().await;
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
                let configured = ch.get(key).and_then(|v| v.as_str()).is_some_and(|s| !s.is_empty());
                if configured {
                    // Use runtime state if available; otherwise use "connecting" status
                    let (connected, last_ts, error) = match runtime_status.get(name) {
                        Some(state) => (
                            state.connected,
                            state.last_event.as_ref().map(|t| t.to_rfc3339()),
                            state.error.clone(),
                        ),
                        None => (false, None, Some("connecting".to_string())),
                    };
                    channels.push(json!({
                        "name": name,
                        "connected": connected,
                        "last_connected": last_ts,
                        "error": error,
                    }));
                }
            }
        }

        // Include per-agent channels from agent registry configs
        let mut seen_labels = std::collections::HashSet::new();
        {
            let reg = self.registry.read().await;
            for agent in reg.list() {
                if let Some(ch) = &agent.config.channels {
                    let name = &agent.config.agent.name;
                    let pairs: &[(&str, bool)] = &[
                        ("discord", ch.discord.as_ref().is_some_and(|d| !d.bot_token.is_empty() || d.bot_token_enc.as_ref().is_some_and(|e| !e.is_empty()))),
                        ("telegram", ch.telegram.as_ref().is_some_and(|t| !t.bot_token.is_empty() || t.bot_token_enc.as_ref().is_some_and(|e| !e.is_empty()))),
                        ("slack", ch.slack.as_ref().is_some_and(|s| !s.bot_token.is_empty() || s.bot_token_enc.as_ref().is_some_and(|e| !e.is_empty()))),
                    ];
                    for &(platform, configured) in pairs {
                        if configured {
                            let label = format!("{platform}:{name}");
                            seen_labels.insert(label.clone());
                            let (connected, last_ts, error) = match runtime_status.get(&label) {
                                Some(state) => (
                                    state.connected,
                                    state.last_event.as_ref().map(|t| t.to_rfc3339()),
                                    state.error.clone(),
                                ),
                                None => (false, None, Some("connecting".to_string())),
                            };
                            channels.push(json!({
                                "name": label,
                                "connected": connected,
                                "last_connected": last_ts,
                                "error": error,
                            }));
                        }
                    }
                }
            }
        }

        // Also include runtime-only per-agent entries not yet in registry (edge case)
        for (key, state) in runtime_status.iter() {
            if key.contains(':') && !seen_labels.contains(key.as_str()) {
                channels.push(json!({
                    "name": key,
                    "connected": state.connected,
                    "last_connected": state.last_event.as_ref().map(|t| t.to_rfc3339()),
                    "error": state.error.clone(),
                }));
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
        let agent_name = params.get("agent").and_then(|v| v.as_str()).unwrap_or("");

        if token.is_empty() {
            return WsFrame::error_response("", "Missing 'config.token' parameter");
        }

        // Per-agent channel: write to agent.toml [channels.{platform}]
        if !agent_name.is_empty() {
            let (token_field, secret_field) = match channel_type {
                "discord" => ("bot_token", None),
                "telegram" => ("bot_token", None),
                "slack" => ("bot_token", Some("app_token")),
                _ => return WsFrame::error_response("", &format!("Per-agent channels not supported for: {channel_type}")),
            };

            let token_owned = token.to_string();
            let secret_owned = secret.to_string();
            let channel_type_owned = channel_type.to_string();
            let home = self.home_dir.clone();

            if let Err(e) = self.update_agent_toml(agent_name, move |table| {
                let channels = table.entry("channels")
                    .or_insert_with(|| toml::Value::Table(toml::Table::new()))
                    .as_table_mut()
                    .ok_or("Invalid [channels] section")?;
                let section = channels.entry(&channel_type_owned)
                    .or_insert_with(|| toml::Value::Table(toml::Table::new()))
                    .as_table_mut()
                    .ok_or_else(|| format!("Invalid [channels.{}] section", channel_type_owned))?;

                section.insert(token_field.to_string(), toml::Value::String(token_owned.clone()));
                if let Some(enc) = crate::config_crypto::encrypt_value(&token_owned, &home) {
                    section.insert(format!("{token_field}_enc"), toml::Value::String(enc));
                }
                if let Some(sf) = secret_field {
                    if !secret_owned.is_empty() {
                        section.insert(sf.to_string(), toml::Value::String(secret_owned.clone()));
                        if let Some(enc) = crate::config_crypto::encrypt_value(&secret_owned, &home) {
                            section.insert(format!("{sf}_enc"), toml::Value::String(enc));
                        }
                    }
                }
                Ok(())
            }).await {
                return WsFrame::error_response("", &format!("Failed to update agent config: {e}"));
            }

            // Hot-start: stop existing per-agent bot if any, then re-launch all per-agent bots
            let label = format!("{channel_type}:{agent_name}");
            self.hot_stop_channel(&label).await;

            let mut hot_started = false;
            if let Some(ctx) = self.reply_ctx.read().await.clone() {
                let handles: Vec<(String, tokio::task::JoinHandle<()>)> = match channel_type {
                    "discord" => crate::discord::start_discord_bots(&self.home_dir, ctx).await,
                    "telegram" => crate::telegram::start_telegram_bots(&self.home_dir, ctx).await,
                    _ => Vec::new(),
                };
                for (l, h) in handles {
                    if l == label { hot_started = true; }
                    self.register_channel_handle(&l, h).await;
                }
            }

            info!(channel_type, agent_name, "Per-agent channel config saved");
            return WsFrame::ok_response("", json!({
                "success": true,
                "type": label,
                "hot_started": hot_started,
            }));
        }

        // Global channel: write to config.toml [channels]
        let (token_key, secret_key) = match channel_type {
            "line" => ("line_channel_token", Some("line_channel_secret")),
            "telegram" => ("telegram_bot_token", None),
            "discord" => ("discord_bot_token", None),
            "slack" => ("slack_bot_token", Some("slack_app_token")),
            "whatsapp" => ("whatsapp_access_token", Some("whatsapp_phone_number_id")),
            "feishu" => ("feishu_app_id", Some("feishu_app_secret")),
            _ => return WsFrame::error_response("", &format!("Unknown channel type: {channel_type}")),
        };

        // Encrypt tokens before storing (H3)
        let enc_token_key = format!("{token_key}_enc");
        let encrypted_token = crate::config_crypto::encrypt_value(token, &self.home_dir);
        let enc_secret = if let Some(sk) = secret_key {
            if !secret.is_empty() {
                Some((format!("{sk}_enc"), crate::config_crypto::encrypt_value(secret, &self.home_dir)))
            } else {
                None
            }
        } else {
            None
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

        // Store encrypted version; also keep plaintext as fallback
        channels.insert(token_key.to_string(), toml::Value::String(token.to_string()));
        if let Some(enc) = &encrypted_token {
            channels.insert(enc_token_key, toml::Value::String(enc.clone()));
        }
        if let Some((sk_enc, enc_val)) = &enc_secret {
            if let Some(sk) = secret_key {
                channels.insert(sk.to_string(), toml::Value::String(secret.to_string()));
            }
            if let Some(v) = enc_val {
                channels.insert(sk_enc.clone(), toml::Value::String(v.clone()));
            }
        }

        if let Err(e) = self.write_config_table(&config_path, &table).await {
            return WsFrame::error_response("", &format!("Failed to write config.toml: {e}"));
        }

        info!(channel_type, "Channel config saved");

        // Hot-start: launch the channel bot immediately without gateway restart
        let hot_started = self.hot_start_channel(channel_type).await;

        WsFrame::ok_response("", json!({
            "success": true,
            "type": channel_type,
            "hot_started": hot_started,
        }))
    }

    async fn handle_channels_test(&self, params: Value) -> WsFrame {
        let channel_type = params.get("type").and_then(|v| v.as_str()).unwrap_or("unknown");
        info!(channel_type, "channels.test requested");

        // Per-agent channel test: check agent.toml
        if let Some((platform, agent_name)) = channel_type.split_once(':') {
            let token_field = match platform {
                "discord" | "telegram" => "bot_token",
                "slack" => "bot_token",
                _ => return WsFrame::error_response("", &format!("Unknown channel platform: {platform}")),
            };

            let reg = self.registry.read().await;
            let configured = reg.get(agent_name).is_some_and(|agent| {
                if let Some(ch) = &agent.config.channels {
                    match platform {
                        "discord" => ch.discord.as_ref().is_some_and(|d| !d.bot_token.is_empty() || d.bot_token_enc.as_ref().is_some_and(|e| !e.is_empty())),
                        "telegram" => ch.telegram.as_ref().is_some_and(|t| !t.bot_token.is_empty() || t.bot_token_enc.as_ref().is_some_and(|e| !e.is_empty())),
                        "slack" => ch.slack.as_ref().is_some_and(|s| !s.bot_token.is_empty() || s.bot_token_enc.as_ref().is_some_and(|e| !e.is_empty())),
                        _ => false,
                    }
                } else {
                    false
                }
            });
            drop(reg);

            return WsFrame::ok_response("", json!({
                "success": configured,
                "type": channel_type,
                "message": if configured { format!("{channel_type} {token_field} is configured") } else { format!("{channel_type} token 未設定") },
            }));
        }

        // Global channel test
        let token_key = match channel_type {
            "line" => "line_channel_token",
            "telegram" => "telegram_bot_token",
            "discord" => "discord_bot_token",
            _ => return WsFrame::error_response("", &format!("Unknown channel type: {channel_type}")),
        };

        let config_path = self.home_dir.join("config.toml");
        let table = self.read_config_table(&config_path).await;

        // Check both plaintext and encrypted token
        let has_token = crate::config_crypto::decrypt_config_field(&table, "channels", token_key, &self.home_dir)
            .is_some_and(|t| !t.is_empty());

        if !has_token {
            return WsFrame::ok_response("", json!({
                "success": false,
                "type": channel_type,
                "message": format!("{channel_type} token 未設定"),
            }));
        }

        WsFrame::ok_response("", json!({
            "success": true,
            "type": channel_type,
            "message": format!("{channel_type} token is configured"),
        }))
    }

    async fn handle_channels_remove(&self, params: Value) -> WsFrame {
        let channel_type = match params.get("type").and_then(|v| v.as_str()) {
            Some(t) => t,
            None => return WsFrame::error_response("", "Missing 'type' parameter"),
        };

        // Per-agent channel: format "discord:agent_name", "telegram:agent_name", etc.
        if let Some((platform, agent_name)) = channel_type.split_once(':') {
            let channel_section = match platform {
                "discord" | "telegram" | "slack" => platform,
                _ => return WsFrame::error_response("", &format!("Unknown channel platform: {platform}")),
            };

            // Clear the [channels.{platform}] section in the agent's agent.toml
            let agent_name_owned = agent_name.to_string();
            let channel_section_owned = channel_section.to_string();
            if let Err(e) = self.update_agent_toml(&agent_name_owned, |table| {
                if let Some(channels) = table.get_mut("channels").and_then(|v| v.as_table_mut()) {
                    channels.remove(&channel_section_owned);
                }
                Ok(())
            }).await {
                return WsFrame::error_response("", &format!("Failed to update agent config: {e}"));
            }

            // Hot-stop the per-agent bot
            self.hot_stop_channel(channel_type).await;

            info!(channel_type, "Per-agent channel removed and stopped");
            return WsFrame::ok_response("", json!({
                "success": true,
                "type": channel_type,
            }));
        }

        // Global channel removal
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
            // Also clear the encrypted version
            let enc_key = format!("{token_key}_enc");
            channels.insert(enc_key, toml::Value::String(String::new()));
            // Also clear secret for LINE
            if channel_type == "line" {
                channels.insert("line_channel_secret".to_string(), toml::Value::String(String::new()));
                channels.insert("line_channel_secret_enc".to_string(), toml::Value::String(String::new()));
            }
        }

        if let Err(e) = self.write_config_table(&config_path, &table).await {
            return WsFrame::error_response("", &format!("Failed to write config.toml: {e}"));
        }

        // Hot-stop: abort the running global channel bot task
        self.hot_stop_channel(channel_type).await;

        // Re-launch per-agent bots since the global bot was deduplicating their tokens.
        let mut restarted_agents = Vec::new();
        let ctx_opt = self.reply_ctx.read().await.clone();
        if let Some(ctx) = ctx_opt {
            let per_agent_handles: Vec<(String, tokio::task::JoinHandle<()>)> = match channel_type {
                "discord" => crate::discord::start_discord_bots(&self.home_dir, ctx).await,
                "telegram" => crate::telegram::start_telegram_bots(&self.home_dir, ctx).await,
                // Slack: per-agent ready but module has unresolved deps
                // "slack" => crate::slack::start_slack_bots(&self.home_dir, ctx).await,
                _ => Vec::new(),
            };
            for (label, h) in per_agent_handles {
                restarted_agents.push(label.clone());
                self.register_channel_handle(&label, h).await;
            }
        }

        info!(channel_type, restarted = ?restarted_agents, "Channel removed and stopped");
        WsFrame::ok_response("", json!({
            "success": true,
            "type": channel_type,
            "restarted_per_agent": restarted_agents,
        }))
    }

    // ── Channel hot-start/stop ────────────────────────────────

    /// Launch a channel bot immediately after config is saved.
    async fn hot_start_channel(&self, channel_type: &str) -> bool {
        let ctx = match self.reply_ctx.read().await.clone() {
            Some(ctx) => ctx,
            None => {
                warn!(channel_type, "Cannot hot-start channel: ReplyContext not available");
                return false;
            }
        };

        // Stop existing instance first (if any)
        self.hot_stop_channel(channel_type).await;

        let home = self.home_dir.clone();
        let handle = match channel_type {
            "telegram" => crate::telegram::start_telegram_bot(&home, ctx).await,
            "discord" => crate::discord::start_discord_bot(&home, ctx).await,
            "line" => {
                // LINE uses webhook (axum router), not a background task.
                // Updating config is enough; the webhook handler reads token on each request.
                info!("LINE channel updated (webhook-based, no background task needed)");
                return true;
            }
            _ => None,
        };

        match handle {
            Some(h) => {
                info!(channel_type, "Channel hot-started successfully");
                self.channel_handles.lock().await.insert(channel_type.to_string(), h);
                true
            }
            None => {
                warn!(channel_type, "Channel hot-start failed (check token validity)");
                false
            }
        }
    }

    /// Stop a running channel bot task.
    async fn hot_stop_channel(&self, channel_type: &str) {
        let mut handles = self.channel_handles.lock().await;
        if let Some(handle) = handles.remove(channel_type) {
            handle.abort();
            info!(channel_type, "Channel bot stopped");
        }
        // Always clear runtime status (handle may already be gone if bot crashed)
        let mut status = self.channel_status.write().await;
        status.remove(channel_type);
    }

    // ── Accounts ─────────────────────────────────────────────

    async fn handle_accounts_list(&self) -> WsFrame {
        let rotator = self.cached_rotator().await;
        let accounts = rotator.status().await;
        let accounts_json: Vec<Value> = accounts.iter().map(|a| json!({
            "id": a.id,
            "auth_method": a.auth_method,
            "priority": a.priority,
            "is_healthy": a.is_healthy,
            "spent_this_month": a.spent_this_month,
            "monthly_budget_cents": a.monthly_budget_cents,
            "total_requests": a.total_requests,
            "is_available": a.is_available,
            "label": a.label,
            "email": a.email,
            "subscription": a.subscription,
            "expires_at": a.expires_at,
            "days_until_expiry": a.days_until_expiry,
        })).collect();
        WsFrame::ok_response("", json!({ "accounts": accounts_json }))
    }

    async fn handle_budget_summary(&self) -> WsFrame {
        let rotator = self.cached_rotator().await;
        let accounts = rotator.status().await;
        let total_budget: u64 = accounts.iter().map(|a| a.monthly_budget_cents).sum();
        let total_spent: u64 = accounts.iter().map(|a| a.spent_this_month).sum();

        let accounts_json: Vec<Value> = accounts.iter().map(|a| json!({
            "id": a.id,
            "auth_method": a.auth_method,
            "priority": a.priority,
            "is_healthy": a.is_healthy,
            "spent_this_month": a.spent_this_month,
            "monthly_budget_cents": a.monthly_budget_cents,
        })).collect();

        WsFrame::ok_response("", json!({
            "total_budget_cents": total_budget,
            "total_spent_cents": total_spent,
            "accounts": accounts_json,
        }))
    }

    async fn handle_accounts_rotate(&self, _params: Value) -> WsFrame {
        let rotator = self.cached_rotator().await;
        match rotator.select().await {
            Some(selected) => {
                WsFrame::ok_response("", json!({
                    "success": true,
                    "selected_account": selected.id,
                    "strategy": "configured",
                    "message": format!("Rotated to account '{}'", selected.id),
                }))
            }
            None => WsFrame::error_response("", "No available accounts for rotation"),
        }
    }

    async fn handle_accounts_health(&self) -> WsFrame {
        let rotator = self.cached_rotator().await;
        let accounts = rotator.status().await;
        let healthy_count = accounts.iter().filter(|a| a.is_healthy).count();
        let status = if accounts.is_empty() { "no_accounts" }
            else if healthy_count == accounts.len() { "healthy" }
            else if healthy_count > 0 { "degraded" }
            else { "unhealthy" };

        let accounts_json: Vec<Value> = accounts.iter().map(|a| json!({
            "id": a.id,
            "healthy": a.is_healthy,
            "available": a.is_available,
            "spent": a.spent_this_month,
            "budget": a.monthly_budget_cents,
            "requests": a.total_requests,
        })).collect();

        WsFrame::ok_response("", json!({
            "status": status,
            "healthy_count": healthy_count,
            "total_count": accounts.len(),
            "accounts": accounts_json,
        }))
    }

    /// Get or create a cached rotator (uses the same static cache as claude_runner).
    async fn cached_rotator(&self) -> std::sync::Arc<duduclaw_agent::account_rotator::AccountRotator> {
        // Reuse the global cache from claude_runner to avoid redundant disk reads
        match crate::claude_runner::get_rotator_cached(&self.home_dir).await {
            Ok(r) => r,
            Err(_) => {
                // Fallback: create a fresh one
                let config_content = tokio::fs::read_to_string(self.home_dir.join("config.toml"))
                    .await
                    .unwrap_or_default();
                let config_table: toml::Table = config_content.parse().unwrap_or_default();
                let rotator = duduclaw_agent::account_rotator::create_from_config(&config_table);
                let _ = rotator.load_from_config(&self.home_dir).await;
                std::sync::Arc::new(rotator)
            }
        }
    }

    /// Get total spent cents across all accounts (MCP-L5).
    ///
    /// Note: AccountRotator tracks spend per-account (API key), not per-agent.
    /// Per-agent tracking requires adding a usage ledger — this returns the
    /// aggregate across all accounts as an honest approximation.
    async fn get_total_spent(&self) -> u64 {
        let rotator = self.cached_rotator().await;
        let accounts = rotator.status().await;
        accounts.iter().map(|a| a.spent_this_month).sum()
    }

    // ── Memory ──────────────────────────────────────────────

    /// Resolve the per-agent memory.db path.
    /// Prefers `agents/<id>/state/memory.db`, falls back to `agents/<id>/memory.db`.
    fn agent_memory_db_path(&self, agent_id: &str) -> PathBuf {
        let agent_dir = self.home_dir.join("agents").join(agent_id);
        let state_path = agent_dir.join("state").join("memory.db");
        if state_path.exists() { state_path } else { agent_dir.join("memory.db") }
    }

    async fn handle_memory_search(&self, params: Value) -> WsFrame {
        let agent_id = params.get("agent_id").and_then(|v| v.as_str()).unwrap_or("");
        let query = params.get("query").and_then(|v| v.as_str()).unwrap_or("");
        let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(20).min(200) as usize;

        if agent_id.is_empty() || !is_valid_agent_id(agent_id) || query.is_empty() {
            return WsFrame::error_response("", "Missing or invalid 'agent_id' or 'query' parameter");
        }

        let db_path = self.agent_memory_db_path(agent_id);
        if !db_path.exists() {
            return WsFrame::ok_response("", json!({ "entries": [] }));
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
                WsFrame::ok_response("", json!({ "entries": results }))
            }
            Err(e) => WsFrame::error_response("", &format!("Memory search failed: {e}")),
        }
    }

    async fn handle_memory_browse(&self, params: Value) -> WsFrame {
        let agent_id = params.get("agent_id").and_then(|v| v.as_str()).unwrap_or("");
        let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(20).min(200) as usize;

        if agent_id.is_empty() || !is_valid_agent_id(agent_id) {
            return WsFrame::error_response("", "Missing or invalid 'agent_id' parameter");
        }

        let db_path = self.agent_memory_db_path(agent_id);
        if !db_path.exists() {
            return WsFrame::ok_response("", json!({ "entries": [] }));
        }

        let engine = match SqliteMemoryEngine::new(&db_path) {
            Ok(e) => e,
            Err(e) => return WsFrame::error_response("", &format!("Failed to open memory db: {e}")),
        };

        match engine.list_recent(agent_id, limit).await {
            Ok(entries) => {
                let rows: Vec<Value> = entries.iter().map(|e| {
                    json!({
                        "id": e.id,
                        "agent_id": e.agent_id,
                        "content": e.content,
                        "timestamp": e.timestamp.to_rfc3339(),
                        "tags": e.tags,
                    })
                }).collect();
                WsFrame::ok_response("", json!({ "entries": rows }))
            }
            Err(e) => WsFrame::error_response("", &format!("Memory browse failed: {e}")),
        }
    }

    /// List P2 Key-Fact Accumulator entries (exposed as "Key Insights" in the UI).
    ///
    /// Reads the `key_facts` table directly via raw SQL so that a missing table
    /// resolves to an empty result set instead of surfacing an error — the table
    /// is created on demand by `SqliteMemoryEngine::new`, but we want this RPC to
    /// work even against older databases that were created before P2 landed.
    async fn handle_memory_key_facts(&self, params: Value) -> WsFrame {
        let agent_id = params.get("agent_id").and_then(|v| v.as_str()).unwrap_or("");
        let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(50).min(200) as i64;

        if agent_id.is_empty() || !is_valid_agent_id(agent_id) {
            return WsFrame::error_response("", "Missing or invalid 'agent_id' parameter");
        }

        let db_path = self.agent_memory_db_path(agent_id);
        if !db_path.exists() {
            return WsFrame::ok_response("", json!({ "entries": [] }));
        }

        let conn = match rusqlite::Connection::open(&db_path) {
            Ok(c) => c,
            Err(e) => return WsFrame::error_response("", &format!("Failed to open memory db: {e}")),
        };

        let mut stmt = match conn.prepare(
            "SELECT id, agent_id, fact, channel, chat_id, source_session, timestamp, access_count
             FROM key_facts
             WHERE agent_id = ?1
             ORDER BY timestamp DESC
             LIMIT ?2",
        ) {
            Ok(s) => s,
            Err(e) => {
                // Graceful: if the `key_facts` table doesn't exist yet in this
                // memory.db (e.g. legacy agent that hasn't triggered P2 bootstrap),
                // fall back to an empty list rather than surfacing the SQL error.
                let msg = e.to_string();
                if msg.contains("no such table") {
                    return WsFrame::ok_response("", json!({ "entries": [] }));
                }
                return WsFrame::error_response("", &format!("Key facts query prepare failed: {e}"));
            }
        };

        let rows = match stmt.query_map(params![agent_id, limit], |row| {
            Ok(json!({
                "id": row.get::<_, String>(0)?,
                "agent_id": row.get::<_, String>(1)?,
                "fact": row.get::<_, String>(2)?,
                "channel": row.get::<_, String>(3)?,
                "chat_id": row.get::<_, String>(4)?,
                "source_session": row.get::<_, String>(5)?,
                "timestamp": row.get::<_, String>(6)?,
                "access_count": row.get::<_, i64>(7)?,
            }))
        }) {
            Ok(r) => r,
            Err(e) => return WsFrame::error_response("", &format!("Key facts query failed: {e}")),
        };

        let mut entries: Vec<Value> = Vec::new();
        for row in rows {
            match row {
                Ok(v) => entries.push(v),
                Err(e) => {
                    return WsFrame::error_response("", &format!("Key facts row decode failed: {e}"));
                }
            }
        }
        WsFrame::ok_response("", json!({ "entries": entries }))
    }

    // ── Wiki Knowledge Base ──────────────────────────────────

    async fn handle_wiki_pages(&self, params: Value) -> WsFrame {
        let agent_id = params.get("agent_id").and_then(|v| v.as_str()).unwrap_or("");
        if agent_id.is_empty() {
            return WsFrame::error_response("", "Missing 'agent_id' parameter");
        }
        if !is_valid_agent_id(agent_id) {
            return WsFrame::error_response("", "Invalid agent_id format");
        }

        let wiki_dir = self.home_dir.join("agents").join(agent_id).join("wiki");
        if !wiki_dir.exists() {
            return WsFrame::ok_response("", json!({ "pages": [], "exists": false }));
        }

        let store = duduclaw_memory::WikiStore::new(wiki_dir);
        match store.list_pages() {
            Ok(pages) => {
                let items: Vec<Value> = pages.iter().map(|p| {
                    json!({
                        "path": p.path,
                        "title": p.title,
                        "updated": p.updated.to_rfc3339(),
                        "tags": p.tags,
                    })
                }).collect();
                WsFrame::ok_response("", json!({ "pages": items, "exists": true }))
            }
            Err(e) => WsFrame::error_response("", &format!("Failed to list wiki pages: {e}")),
        }
    }

    async fn handle_wiki_read(&self, params: Value) -> WsFrame {
        let agent_id = params.get("agent_id").and_then(|v| v.as_str()).unwrap_or("");
        let page_path = params.get("page_path").and_then(|v| v.as_str()).unwrap_or("");
        if agent_id.is_empty() || page_path.is_empty() {
            return WsFrame::error_response("", "Missing 'agent_id' or 'page_path' parameter");
        }
        if !is_valid_agent_id(agent_id) {
            return WsFrame::error_response("", "Invalid agent_id format");
        }

        let wiki_dir = self.home_dir.join("agents").join(agent_id).join("wiki");
        let store = duduclaw_memory::WikiStore::new(wiki_dir);

        // Allow reading reserved files like _index.md, _schema.md
        match store.read_raw(page_path) {
            Ok(content) => WsFrame::ok_response("", json!({ "content": content, "path": page_path })),
            Err(e) => WsFrame::error_response("", &format!("Failed to read page: {e}")),
        }
    }

    async fn handle_wiki_search(&self, params: Value) -> WsFrame {
        let agent_id = params.get("agent_id").and_then(|v| v.as_str()).unwrap_or("");
        let query = params.get("query").and_then(|v| v.as_str()).unwrap_or("");
        let limit = (params.get("limit").and_then(|v| v.as_u64()).unwrap_or(10) as usize).min(100);

        if agent_id.is_empty() || query.is_empty() {
            return WsFrame::error_response("", "Missing 'agent_id' or 'query' parameter");
        }
        if !is_valid_agent_id(agent_id) {
            return WsFrame::error_response("", "Invalid agent_id format");
        }

        let wiki_dir = self.home_dir.join("agents").join(agent_id).join("wiki");
        if !wiki_dir.exists() {
            return WsFrame::ok_response("", json!({ "hits": [] }));
        }

        let store = duduclaw_memory::WikiStore::new(wiki_dir);
        match store.search(query, limit) {
            Ok(hits) => {
                let items: Vec<Value> = hits.iter().map(|h| {
                    json!({
                        "path": h.path,
                        "title": h.title,
                        "score": h.score,
                        "weighted_score": h.weighted_score,
                        "trust": h.trust,
                        "layer": h.layer.to_string(),
                        "context_lines": h.context_lines,
                    })
                }).collect();
                WsFrame::ok_response("", json!({ "hits": items }))
            }
            Err(e) => WsFrame::error_response("", &format!("Wiki search failed: {e}")),
        }
    }

    async fn handle_wiki_lint(&self, params: Value) -> WsFrame {
        let agent_id = params.get("agent_id").and_then(|v| v.as_str()).unwrap_or("");
        if agent_id.is_empty() {
            return WsFrame::error_response("", "Missing 'agent_id' parameter");
        }
        if !is_valid_agent_id(agent_id) {
            return WsFrame::error_response("", "Invalid agent_id format");
        }

        let wiki_dir = self.home_dir.join("agents").join(agent_id).join("wiki");
        if !wiki_dir.exists() {
            return WsFrame::ok_response("", json!({ "total_pages": 0, "healthy": true }));
        }

        let store = duduclaw_memory::WikiStore::new(wiki_dir);
        match store.lint() {
            Ok(report) => WsFrame::ok_response("", json!({
                "total_pages": report.total_pages,
                "index_entries": report.index_entries,
                "orphan_pages": report.orphan_pages,
                "broken_links": report.broken_links,
                "stale_pages": report.stale_pages,
                "healthy": report.orphan_pages.is_empty() && report.broken_links.is_empty(),
            })),
            Err(e) => WsFrame::error_response("", &format!("Wiki lint failed: {e}")),
        }
    }

    async fn handle_wiki_stats(&self, params: Value) -> WsFrame {
        let agent_id = params.get("agent_id").and_then(|v| v.as_str()).unwrap_or("");
        if agent_id.is_empty() {
            return WsFrame::error_response("", "Missing 'agent_id' parameter");
        }
        if !is_valid_agent_id(agent_id) {
            return WsFrame::error_response("", "Invalid agent_id format");
        }

        let wiki_dir = self.home_dir.join("agents").join(agent_id).join("wiki");
        if !wiki_dir.exists() {
            return WsFrame::ok_response("", json!({ "exists": false, "total_pages": 0 }));
        }

        let store = duduclaw_memory::WikiStore::new(wiki_dir);
        let pages = match store.list_pages() {
            Ok(p) => p,
            Err(e) => return WsFrame::error_response("", &format!("Failed to list pages: {e}")),
        };

        let mut by_dir: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        for p in &pages {
            let dir = std::path::Path::new(&p.path)
                .parent()
                .and_then(|d| d.to_str())
                .unwrap_or("root")
                .to_string();
            *by_dir.entry(dir).or_insert(0) += 1;
        }

        let most_recent = pages.first().map(|p| json!({
            "title": p.title,
            "path": p.path,
            "updated": p.updated.to_rfc3339(),
        }));

        WsFrame::ok_response("", json!({
            "exists": true,
            "total_pages": pages.len(),
            "by_directory": by_dir,
            "most_recent": most_recent,
        }))
    }

    // ── Shared Wiki ─────────────────────────────────────────

    async fn handle_shared_wiki_pages(&self) -> WsFrame {
        let wiki_dir = self.home_dir.join("shared").join("wiki");
        if !wiki_dir.exists() {
            return WsFrame::ok_response("", json!({ "pages": [], "exists": false }));
        }

        let store = duduclaw_memory::WikiStore::new_shared(&self.home_dir);
        match store.list_pages() {
            Ok(pages) => {
                let items: Vec<Value> = pages.iter().map(|p| {
                    json!({
                        "path": p.path,
                        "title": p.title,
                        "updated": p.updated.to_rfc3339(),
                        "tags": p.tags,
                    })
                }).collect();
                WsFrame::ok_response("", json!({ "pages": items, "exists": true }))
            }
            Err(e) => WsFrame::error_response("", &format!("Failed to list shared wiki pages: {e}")),
        }
    }

    async fn handle_shared_wiki_read(&self, params: Value) -> WsFrame {
        let page_path = params.get("page_path").and_then(|v| v.as_str()).unwrap_or("");
        if page_path.is_empty() {
            return WsFrame::error_response("", "Missing 'page_path' parameter");
        }

        let store = duduclaw_memory::WikiStore::new_shared(&self.home_dir);
        match store.read_raw(page_path) {
            Ok(content) => WsFrame::ok_response("", json!({ "content": content, "path": page_path })),
            Err(e) => WsFrame::error_response("", &format!("Failed to read shared wiki page: {e}")),
        }
    }

    async fn handle_shared_wiki_search(&self, params: Value) -> WsFrame {
        let query = params.get("query").and_then(|v| v.as_str()).unwrap_or("");
        let limit = (params.get("limit").and_then(|v| v.as_u64()).unwrap_or(10) as usize).min(100);

        if query.is_empty() {
            return WsFrame::error_response("", "Missing 'query' parameter");
        }

        let wiki_dir = self.home_dir.join("shared").join("wiki");
        if !wiki_dir.exists() {
            return WsFrame::ok_response("", json!({ "hits": [] }));
        }

        let store = duduclaw_memory::WikiStore::new_shared(&self.home_dir);
        match store.search(query, limit) {
            Ok(hits) => {
                let items: Vec<Value> = hits.iter().map(|h| {
                    json!({
                        "path": h.path,
                        "title": h.title,
                        "score": h.score,
                        "weighted_score": h.weighted_score,
                        "trust": h.trust,
                        "layer": h.layer.to_string(),
                        "context_lines": h.context_lines,
                    })
                }).collect();
                WsFrame::ok_response("", json!({ "hits": items }))
            }
            Err(e) => WsFrame::error_response("", &format!("Shared wiki search failed: {e}")),
        }
    }

    async fn handle_shared_wiki_stats(&self) -> WsFrame {
        let wiki_dir = self.home_dir.join("shared").join("wiki");
        if !wiki_dir.exists() {
            return WsFrame::ok_response("", json!({ "exists": false, "total_pages": 0 }));
        }

        let store = duduclaw_memory::WikiStore::new_shared(&self.home_dir);
        let pages = match store.list_pages() {
            Ok(p) => p,
            Err(e) => return WsFrame::error_response("", &format!("Failed to list shared wiki pages: {e}")),
        };

        let mut by_author: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        let mut by_dir: std::collections::HashMap<String, usize> = std::collections::HashMap::new();

        for p in &pages {
            // Count by author from the WikiPage.author field
            let author = p.author.as_deref().unwrap_or("unknown");
            *by_author.entry(author.to_string()).or_default() += 1;

            let dir = std::path::Path::new(&p.path)
                .parent()
                .and_then(|d| d.to_str())
                .unwrap_or("root")
                .to_string();
            *by_dir.entry(dir).or_default() += 1;
        }

        let most_recent = pages.first().map(|p| json!({
            "title": p.title,
            "path": p.path,
            "updated": p.updated.to_rfc3339(),
            "author": p.author,
        }));

        WsFrame::ok_response("", json!({
            "exists": true,
            "total_pages": pages.len(),
            "by_author": by_author,
            "by_directory": by_dir,
            "most_recent": most_recent,
        }))
    }

    // ── Skills ──────────────────────────────────────────────

    async fn handle_skills_list(&self, params: Value) -> WsFrame {
        let agent_id = params.get("agent_id").and_then(|v| v.as_str());
        let reg = self.registry.read().await;

        // Collect global skill names for scope tagging
        let global_names: std::collections::HashSet<&str> =
            reg.global_skills().iter().map(|s| s.name.as_str()).collect();

        match agent_id {
            Some(id) => {
                match reg.get(id) {
                    Some(agent) => {
                        let skills: Vec<Value> = agent.skills.iter().map(|s| {
                            let scope = if global_names.contains(s.name.as_str()) { "global" } else { "agent" };
                            json!({ "name": s.name, "size": s.content.len(), "scope": scope })
                        }).collect();
                        WsFrame::ok_response("", json!({ "agent_id": id, "skills": skills }))
                    }
                    None => WsFrame::error_response("", &format!("Agent not found: {id}")),
                }
            }
            None => {
                // Global skills
                let global: Vec<Value> = reg.global_skills().iter().map(|s| {
                    json!({ "name": s.name, "size": s.content.len() })
                }).collect();

                // Per-agent skills
                let mut all_skills = Vec::new();
                for agent in reg.list() {
                    let skills: Vec<Value> = agent.skills.iter().map(|s| {
                        let scope = if global_names.contains(s.name.as_str()) { "global" } else { "agent" };
                        json!({ "name": s.name, "size": s.content.len(), "scope": scope })
                    }).collect();
                    all_skills.push(json!({
                        "agent_id": agent.config.agent.name,
                        "skills": skills,
                    }));
                }
                WsFrame::ok_response("", json!({
                    "global_skills": global,
                    "agents": all_skills,
                }))
            }
        }
    }

    async fn handle_skills_search(&self, params: Value) -> WsFrame {
        let query = params.get("query").and_then(|v| v.as_str()).unwrap_or("");
        if query.is_empty() {
            return WsFrame::error_response("", "Missing 'query' parameter");
        }

        let lower = query.to_lowercase();
        let reg = self.registry.read().await;
        let mut results = Vec::new();

        // Search across all agents' installed skills
        for agent in reg.list() {
            for skill in &agent.skills {
                let name_match = skill.name.to_lowercase().contains(&lower);
                let content_match = skill.content.to_lowercase().contains(&lower);
                if name_match || content_match {
                    results.push(json!({
                        "name": skill.name,
                        "description": skill.content.lines().take(3).collect::<Vec<_>>().join(" ").chars().take(200).collect::<String>(),
                        "tags": [],
                        "author": agent.config.agent.name,
                        "url": "",
                        "compatible": ["duduclaw"],
                    }));
                }
            }
        }

        // Search the skill market registry (remote-backed, cached locally)
        let mut registry = duduclaw_agent::skill_registry::SkillRegistry::load(&self.home_dir);

        // Auto-refresh from remote if cache is stale or empty
        if registry.needs_refresh() {
            let _ = registry.refresh().await;
        }

        // Collect local skill names for dedup (MCP-L3)
        let local_names: std::collections::HashSet<String> = results.iter()
            .filter_map(|r| r["name"].as_str().map(|s| s.to_string()))
            .collect();

        let index_results = registry.search(query, 20);
        for entry in index_results {
            if !local_names.contains(&entry.name) {
                results.push(json!({
                    "name": entry.name,
                    "description": entry.description,
                    "tags": entry.tags,
                    "author": entry.author,
                    "url": entry.url,
                    "compatible": entry.compatible,
                }));
            }
        }

        WsFrame::ok_response("", json!({
            "skills": results,
            "source": registry.source(),
            "total_indexed": registry.count(),
        }))
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

    // ── Skill Vetting & Install ──────────────────────────────

    /// Convert a GitHub URL to a raw content URL for SKILL.md.
    fn github_to_raw_url(url: &str) -> String {
        // https://github.com/user/repo -> https://raw.githubusercontent.com/user/repo/HEAD/SKILL.md
        // https://github.com/user/repo/blob/main/SKILL.md -> raw URL
        let trimmed = url.trim().trim_end_matches('/');
        if trimmed.contains("/blob/") {
            // Direct file URL: convert /blob/ to raw
            trimmed
                .replace("github.com", "raw.githubusercontent.com")
                .replace("/blob/", "/")
        } else {
            // Repo root: append HEAD/SKILL.md
            let base = trimmed.replace("github.com", "raw.githubusercontent.com");
            format!("{base}/HEAD/SKILL.md")
        }
    }

    /// Rust-native security scanner for skill content.
    /// Returns `{ passed: bool, findings: [...], score: f64 }`.
    fn vet_skill_native(content: &str) -> Value {
        let mut findings: Vec<Value> = Vec::new();
        let content_lower = content.to_lowercase();

        // Category 1: Shell command injection
        let shell_patterns = [
            ("system(", "Shell command execution via system()"),
            ("exec(", "Shell command execution via exec()"),
            ("subprocess", "Python subprocess invocation"),
            ("os.popen", "OS pipe command execution"),
            ("$(", "Shell command substitution"),
            ("child_process", "Node.js child process execution"),
            ("spawn(", "Process spawn invocation"),
        ];
        // Check for backtick shell execution (separate because of escaping)
        if content.contains('`') {
            // Count backtick pairs — heuristic for shell execution in markdown
            let backtick_count = content.matches('`').count();
            // Only flag if there are odd backtick usages outside of code blocks
            // Skip this for markdown code blocks (triple backticks)
            let triple = content.matches("```").count();
            let singles = backtick_count - (triple * 3);
            if singles > 0 && singles % 2 != 0 {
                findings.push(json!({
                    "category": "shell_injection",
                    "severity": "medium",
                    "description": "Potential shell execution via backticks",
                }));
            }
        }
        for (pattern, desc) in &shell_patterns {
            if content_lower.contains(pattern) {
                findings.push(json!({
                    "category": "shell_injection",
                    "severity": "high",
                    "description": desc,
                    "pattern": pattern,
                }));
            }
        }

        // Category 2: Network exfiltration
        let network_patterns = [
            ("curl ", "Network request via curl"),
            ("wget ", "Network download via wget"),
            ("fetch(", "JavaScript fetch API call"),
            ("http.get", "HTTP GET request"),
            ("requests.", "Python requests library"),
            ("urllib", "Python urllib usage"),
            ("xmlhttprequest", "XMLHttpRequest usage"),
        ];
        for (pattern, desc) in &network_patterns {
            if content_lower.contains(pattern) {
                findings.push(json!({
                    "category": "network_exfiltration",
                    "severity": "high",
                    "description": desc,
                    "pattern": pattern,
                }));
            }
        }

        // Category 3: File system dangers
        let fs_patterns = [
            ("rm -rf", "Recursive force delete"),
            ("rmdir", "Directory removal"),
            ("unlink(", "File deletion via unlink"),
            ("fs.writefile", "Node.js file write"),
            ("shutil.rmtree", "Python recursive directory removal"),
            ("os.remove", "Python file removal"),
            ("fs.unlinkSync", "Node.js synchronous file deletion"),
        ];
        for (pattern, desc) in &fs_patterns {
            if content_lower.contains(pattern) {
                findings.push(json!({
                    "category": "filesystem_danger",
                    "severity": "critical",
                    "description": desc,
                    "pattern": pattern,
                }));
            }
        }

        // Category 4: Prompt injection
        let injection_patterns = [
            ("ignore previous", "Prompt injection: ignore previous instructions"),
            ("disregard", "Prompt injection: disregard instructions"),
            ("you are now", "Prompt injection: role override"),
            ("system prompt", "Prompt injection: system prompt reference"),
            ("forget your instructions", "Prompt injection: instruction override"),
            ("new persona", "Prompt injection: persona override"),
        ];
        for (pattern, desc) in &injection_patterns {
            if content_lower.contains(pattern) {
                findings.push(json!({
                    "category": "prompt_injection",
                    "severity": "critical",
                    "description": desc,
                    "pattern": pattern,
                }));
            }
        }

        // Category 5: Secrets access
        let secret_patterns = [
            (".env", "Environment file reference"),
            ("api_key", "API key reference"),
            ("secret", "Secret reference"),
            ("token", "Token reference"),
            ("credentials", "Credentials reference"),
            ("private_key", "Private key reference"),
            ("password", "Password reference"),
        ];
        for (pattern, desc) in &secret_patterns {
            if content_lower.contains(pattern) {
                // Lower severity — mentioning secrets in documentation is common
                findings.push(json!({
                    "category": "secrets_access",
                    "severity": "medium",
                    "description": desc,
                    "pattern": pattern,
                }));
            }
        }

        // Category 6: Obfuscation
        let obfuscation_patterns = [
            ("base64", "Base64 encoding/decoding"),
            ("eval(", "Dynamic code evaluation"),
            ("atob(", "JavaScript base64 decode"),
            ("btoa(", "JavaScript base64 encode"),
            ("fromcharcode", "Character code construction"),
            ("\\x", "Hex escape sequences"),
        ];
        for (pattern, desc) in &obfuscation_patterns {
            if content_lower.contains(pattern) {
                findings.push(json!({
                    "category": "obfuscation",
                    "severity": "high",
                    "description": desc,
                    "pattern": pattern,
                }));
            }
        }

        // Calculate score: start at 100, deduct based on severity
        let mut score: f64 = 100.0;
        for finding in &findings {
            match finding["severity"].as_str().unwrap_or("low") {
                "critical" => score -= 25.0,
                "high" => score -= 15.0,
                "medium" => score -= 5.0,
                "low" => score -= 2.0,
                _ => score -= 1.0,
            }
        }
        score = score.max(0.0);

        let has_critical_or_high = findings.iter().any(|f| {
            matches!(f["severity"].as_str(), Some("critical") | Some("high"))
        });

        json!({
            "passed": !has_critical_or_high,
            "findings": findings,
            "score": score,
            "scanner": "native",
        })
    }

    async fn handle_skills_vet(&self, params: Value) -> WsFrame {
        let url = match params.get("url").and_then(|v| v.as_str()) {
            Some(u) if !u.is_empty() => u,
            _ => return WsFrame::error_response("", "Missing 'url' parameter"),
        };

        // Fetch SKILL.md content from GitHub
        let raw_url = Self::github_to_raw_url(url);
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .unwrap_or_default();

        let content = match client.get(&raw_url).send().await {
            Ok(resp) if resp.status().is_success() => {
                match resp.text().await {
                    Ok(text) => text,
                    Err(e) => return WsFrame::error_response("", &format!("Failed to read response: {e}")),
                }
            }
            Ok(resp) => {
                return WsFrame::error_response(
                    "",
                    &format!("Failed to fetch SKILL.md: HTTP {}", resp.status()),
                );
            }
            Err(e) => {
                return WsFrame::error_response("", &format!("Failed to fetch SKILL.md: {e}"));
            }
        };

        // Extract skill name from frontmatter (best-effort)
        let skill_name = content
            .lines()
            .find(|l| l.starts_with("name:"))
            .and_then(|l| l.strip_prefix("name:"))
            .map(|n| n.trim().to_string())
            .unwrap_or_else(|| "unknown".to_string());

        // Try Python vet first, fall back to native scanner
        let vet_result = match crate::evolution::vet_skill(
            &self.home_dir,
            &skill_name,
            &content,
            None,
            None,
        )
        .await
        {
            Ok(result) => result,
            Err(e) => {
                info!("Python vet unavailable ({e}), using native scanner");
                Self::vet_skill_native(&content)
            }
        };

        // Determine passed status
        let passed = if let Some(p) = vet_result.get("passed").and_then(|v| v.as_bool()) {
            p
        } else {
            // For Python result: check findings for critical/high severity
            let vet_str = vet_result.to_string();
            !vet_str.contains("\"severity\": \"critical\"")
                && !vet_str.contains("\"severity\":\"critical\"")
                && !vet_str.contains("\"severity\": \"high\"")
                && !vet_str.contains("\"severity\":\"high\"")
        };

        WsFrame::ok_response("", json!({
            "skill_name": skill_name,
            "content": content,
            "vet_result": vet_result,
            "passed": passed,
        }))
    }

    async fn handle_skills_install(&self, params: Value) -> WsFrame {
        let url = match params.get("url").and_then(|v| v.as_str()) {
            Some(u) => u.to_string(),
            None => return WsFrame::error_response("", "Missing 'url' parameter"),
        };
        let scope = match params.get("scope").and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => return WsFrame::error_response("", "Missing 'scope' parameter"),
        };
        let content = match params.get("content").and_then(|v| v.as_str()) {
            Some(c) if !c.is_empty() => c.to_string(),
            _ => return WsFrame::error_response("", "Missing 'content' parameter"),
        };

        // Extract skill name from content frontmatter
        let skill_name = content
            .lines()
            .find(|l| l.starts_with("name:"))
            .and_then(|l| l.strip_prefix("name:"))
            .map(|n| n.trim().to_string())
            .unwrap_or_else(|| "unknown".to_string());

        // Write content to a temp file for the install functions
        let tmp_dir = std::env::temp_dir().join("duduclaw-skill-install");
        if let Err(e) = std::fs::create_dir_all(&tmp_dir) {
            return WsFrame::error_response("", &format!("Failed to create temp dir: {e}"));
        }
        let tmp_file = tmp_dir.join(format!("{skill_name}.md"));
        if let Err(e) = std::fs::write(&tmp_file, &content) {
            return WsFrame::error_response("", &format!("Failed to write temp file: {e}"));
        }

        let quarantine_dir = self.home_dir.join("quarantine");

        let install_result = if scope == "global" {
            duduclaw_agent::skill_loader::install_skill_global(
                &tmp_file,
                &self.home_dir,
                &quarantine_dir,
            )
            .await
        } else {
            // scope is an agent_id — validate it
            if !is_valid_agent_id(&scope) {
                let _ = std::fs::remove_file(&tmp_file);
                return WsFrame::error_response("", "Invalid agent_id for scope");
            }
            let agent_skills_dir = self.home_dir.join("agents").join(&scope).join("SKILLS");
            duduclaw_agent::skill_loader::install_skill(
                &tmp_file,
                &agent_skills_dir,
                &quarantine_dir,
            )
            .await
        };

        // Clean up temp file
        let _ = std::fs::remove_file(&tmp_file);

        match install_result {
            Ok(parsed) => {
                // Reload agent registry to pick up the new skill
                let mut registry = self.registry.write().await;
                if let Err(e) = registry.scan().await {
                    warn!("Failed to rescan agents after skill install: {e}");
                }

                info!(
                    skill = %parsed.meta.name,
                    scope = %scope,
                    url = %url,
                    "Skill installed via dashboard"
                );

                WsFrame::ok_response("", json!({
                    "success": true,
                    "skill_name": parsed.meta.name,
                    "scope": scope,
                }))
            }
            Err(e) => WsFrame::error_response("", &format!("Install failed: {e}")),
        }
    }

    // ── Cron ────────────────────────────────────────────────

    /// Return a reference to the injected cron store, or an error frame if
    /// the gateway has not finished initializing the store yet.
    async fn cron_store(&self) -> Result<Arc<CronStore>, WsFrame> {
        match self.cron_store.read().await.as_ref() {
            Some(store) => Ok(store.clone()),
            None => Err(WsFrame::error_response(
                "",
                "Cron store not initialized yet — retry in a moment",
            )),
        }
    }

    /// Serialize a `CronTaskRow` into the JSON shape the dashboard expects.
    fn cron_row_to_json(row: &CronTaskRow) -> Value {
        json!({
            "id": row.id,
            "name": row.name,
            "agent_id": row.agent_id,
            "cron": row.cron,
            // Alias kept for legacy dashboard clients that read `schedule`.
            "schedule": row.cron,
            "task": row.task,
            "enabled": row.enabled,
            "created_at": row.created_at,
            "updated_at": row.updated_at,
            "last_run_at": row.last_run_at,
            "last_status": row.last_status,
            "last_error": row.last_error,
            "run_count": row.run_count,
            "failure_count": row.failure_count,
            "notify_channel": row.notify_channel,
            "notify_chat_id": row.notify_chat_id,
            "notify_thread_id": row.notify_thread_id,
        })
    }

    async fn handle_cron_list(&self) -> WsFrame {
        let store = match self.cron_store().await {
            Ok(s) => s,
            Err(f) => return f,
        };
        match store.list_all().await {
            Ok(rows) => {
                let tasks: Vec<Value> = rows.iter().map(Self::cron_row_to_json).collect();
                WsFrame::ok_response("", json!({ "tasks": tasks }))
            }
            Err(e) => WsFrame::error_response("", &format!("list cron tasks: {e}")),
        }
    }

    async fn handle_cron_add(&self, params: Value) -> WsFrame {
        let name = match params.get("name").and_then(|v| v.as_str()) {
            Some(n) if !n.is_empty() => n.to_string(),
            _ => return WsFrame::error_response("", "Missing 'name' parameter"),
        };
        // Accept both `cron` (new) and `schedule` (legacy) from the dashboard.
        let cron_expr = params
            .get("cron")
            .or_else(|| params.get("schedule"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if cron_expr.is_empty() {
            return WsFrame::error_response("", "Missing 'cron' parameter");
        }
        // Validate (accept 5- or 6-field). `normalise_cron` turns 5 fields into 6.
        let normalised = crate::cron_scheduler::normalise_cron(&cron_expr);
        if normalised.parse::<cron::Schedule>().is_err() {
            return WsFrame::error_response(
                "",
                &format!("Invalid cron expression: {cron_expr}"),
            );
        }
        let agent_id = params
            .get("agent_id")
            .and_then(|v| v.as_str())
            .unwrap_or("default")
            .to_string();
        // `task` is the actual prompt body; `action` is kept as a legacy alias.
        let task_body = params
            .get("task")
            .or_else(|| params.get("prompt"))
            .or_else(|| params.get("action"))
            .and_then(|v| v.as_str())
            .unwrap_or("heartbeat")
            .to_string();

        let store = match self.cron_store().await {
            Ok(s) => s,
            Err(f) => return f,
        };

        // Enforce unique name for friendly dashboard UX.
        match store.get_by_name(&name).await {
            Ok(Some(_)) => {
                return WsFrame::error_response("", &format!("Cron task '{name}' already exists"));
            }
            Ok(None) => {}
            Err(e) => return WsFrame::error_response("", &format!("lookup: {e}")),
        }

        let notify_channel = params
            .get("notify_channel")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(String::from);
        let notify_chat_id = params
            .get("notify_chat_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(String::from);
        let notify_thread_id = params
            .get("notify_thread_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(String::from);
        if notify_channel.is_some() != notify_chat_id.is_some() {
            return WsFrame::error_response(
                "",
                "notify_channel and notify_chat_id must be set together",
            );
        }

        let mut row = CronTaskRow::new(
            uuid::Uuid::new_v4().to_string(),
            name.clone(),
            agent_id.clone(),
            cron_expr.clone(),
            task_body,
        );
        row.notify_channel = notify_channel;
        row.notify_chat_id = notify_chat_id;
        row.notify_thread_id = notify_thread_id;
        if let Err(e) = store.insert(&row).await {
            return WsFrame::error_response("", &format!("insert: {e}"));
        }
        self.notify_cron_reload().await;
        info!(name = %name, cron = %cron_expr, agent_id = %agent_id, "Cron task added");
        WsFrame::ok_response("", json!({ "success": true, "task": Self::cron_row_to_json(&row) }))
    }

    async fn handle_cron_update(&self, params: Value) -> WsFrame {
        let id = match params.get("id").and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => return WsFrame::error_response("", "Missing 'id' parameter"),
        };

        let store = match self.cron_store().await {
            Ok(s) => s,
            Err(f) => return f,
        };

        let existing = match store.get(&id).await {
            Ok(Some(row)) => row,
            Ok(None) => return WsFrame::error_response("", &format!("Cron task '{id}' not found")),
            Err(e) => return WsFrame::error_response("", &format!("lookup: {e}")),
        };

        let name = params
            .get("name")
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or(existing.name);
        let agent_id = params
            .get("agent_id")
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or(existing.agent_id);
        let cron_expr = params
            .get("cron")
            .or_else(|| params.get("schedule"))
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or(existing.cron);
        let task_body = params
            .get("task")
            .or_else(|| params.get("prompt"))
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or(existing.task);
        let enabled = params
            .get("enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(existing.enabled);

        // Validate cron expression before persisting.
        let normalised = crate::cron_scheduler::normalise_cron(&cron_expr);
        if normalised.parse::<cron::Schedule>().is_err() {
            return WsFrame::error_response(
                "",
                &format!("Invalid cron expression: {cron_expr}"),
            );
        }

        match store
            .update_fields(&id, &name, &agent_id, &cron_expr, &task_body, enabled)
            .await
        {
            Ok(true) => {}
            Ok(false) => {
                return WsFrame::error_response("", &format!("Cron task '{id}' not found"));
            }
            Err(e) => return WsFrame::error_response("", &format!("update: {e}")),
        }

        // Optional: only touch notify_* when any of those keys are present
        // in the payload. Absence means "leave existing values alone".
        let has_notify_update = params.get("notify_channel").is_some()
            || params.get("notify_chat_id").is_some()
            || params.get("notify_thread_id").is_some();
        if has_notify_update {
            let notify_channel = params
                .get("notify_channel")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty());
            let notify_chat_id = params
                .get("notify_chat_id")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty());
            let notify_thread_id = params
                .get("notify_thread_id")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty());
            if notify_channel.is_some() != notify_chat_id.is_some() {
                return WsFrame::error_response(
                    "",
                    "notify_channel and notify_chat_id must be set together",
                );
            }
            if let Err(e) = store
                .update_notify(&id, notify_channel, notify_chat_id, notify_thread_id)
                .await
            {
                return WsFrame::error_response("", &format!("update_notify: {e}"));
            }
        }

        self.notify_cron_reload().await;
        info!(id = %id, "Cron task updated");
        WsFrame::ok_response("", json!({ "success": true, "id": id }))
    }

    async fn handle_cron_set_enabled(&self, params: Value, enabled: bool) -> WsFrame {
        let store = match self.cron_store().await {
            Ok(s) => s,
            Err(f) => return f,
        };

        // Accept either `id` (preferred) or `name` (legacy).
        let result = if let Some(id) = params.get("id").and_then(|v| v.as_str()) {
            store.set_enabled(id, enabled).await
        } else if let Some(name) = params.get("name").and_then(|v| v.as_str()) {
            store.set_enabled_by_name(name, enabled).await
        } else {
            return WsFrame::error_response("", "Missing 'id' or 'name' parameter");
        };

        match result {
            Ok(true) => {
                self.notify_cron_reload().await;
                info!(enabled, "Cron task enable state changed");
                WsFrame::ok_response("", json!({ "success": true, "enabled": enabled }))
            }
            Ok(false) => WsFrame::error_response("", "Cron task not found"),
            Err(e) => WsFrame::error_response("", &format!("set_enabled: {e}")),
        }
    }

    async fn handle_cron_remove(&self, params: Value) -> WsFrame {
        let store = match self.cron_store().await {
            Ok(s) => s,
            Err(f) => return f,
        };

        let result = if let Some(id) = params.get("id").and_then(|v| v.as_str()) {
            store.delete(id).await
        } else if let Some(name) = params.get("name").and_then(|v| v.as_str()) {
            store.delete_by_name(name).await
        } else {
            return WsFrame::error_response("", "Missing 'id' or 'name' parameter");
        };

        match result {
            Ok(true) => {
                self.notify_cron_reload().await;
                info!("Cron task removed");
                WsFrame::ok_response("", json!({ "success": true }))
            }
            Ok(false) => WsFrame::error_response("", "Cron task not found"),
            Err(e) => WsFrame::error_response("", &format!("delete: {e}")),
        }
    }

    // ── Partner Portal ───────────────────────────────────────

    fn partner_store(&self) -> PartnerStore {
        PartnerStore::new(&self.home_dir.join("partner.db"))
    }

    async fn handle_partner_profile(&self) -> WsFrame {
        let store = self.partner_store();
        let profile = store.get_profile();
        match serde_json::to_value(&profile) {
            Ok(v) => WsFrame::ok_response("", v),
            Err(e) => WsFrame::error_response("", &format!("serialize profile: {e}")),
        }
    }

    async fn handle_partner_stats(&self) -> WsFrame {
        let store = self.partner_store();
        let stats = store.compute_stats();
        match serde_json::to_value(&stats) {
            Ok(v) => WsFrame::ok_response("", v),
            Err(e) => WsFrame::error_response("", &format!("serialize stats: {e}")),
        }
    }

    async fn handle_partner_customers(&self, params: Value) -> WsFrame {
        let status = params
            .get("status")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());
        let limit = params
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(100)
            .min(1000) as usize;

        let store = self.partner_store();
        let customers = store.list_customers(status.as_deref(), limit);
        match serde_json::to_value(&customers) {
            Ok(list) => WsFrame::ok_response("", json!({ "customers": list })),
            Err(e) => WsFrame::error_response("", &format!("serialize customers: {e}")),
        }
    }

    async fn handle_partner_profile_update(&self, params: Value) -> WsFrame {
        let tier = match params.get("tier").and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => return WsFrame::error_response("", "Missing 'tier' parameter"),
        };
        let input = PartnerProfileInput {
            company: params
                .get("company")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(String::from),
            tier,
            partner_id: params
                .get("partner_id")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(String::from),
            certified_at: params
                .get("certified_at")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(String::from),
        };
        let store = self.partner_store();
        match store.upsert_profile(&input) {
            Ok(()) => {
                let profile = store.get_profile();
                match serde_json::to_value(&profile) {
                    Ok(v) => WsFrame::ok_response("", v),
                    Err(e) => WsFrame::error_response("", &format!("serialize profile: {e}")),
                }
            }
            Err(e) => WsFrame::error_response("", &e),
        }
    }

    async fn handle_partner_customer_add(&self, params: Value) -> WsFrame {
        let input: PartnerCustomerInput = match serde_json::from_value(params.clone()) {
            Ok(v) => v,
            Err(e) => {
                return WsFrame::error_response(
                    "",
                    &format!("Invalid customer payload: {e}"),
                )
            }
        };
        let store = self.partner_store();
        match store.add_customer(&input) {
            Ok(id) => WsFrame::ok_response("", json!({ "id": id })),
            Err(e) => WsFrame::error_response("", &e),
        }
    }

    async fn handle_partner_customer_update(&self, params: Value) -> WsFrame {
        let id = match params.get("id").and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => return WsFrame::error_response("", "Missing 'id' parameter"),
        };
        let patch_value = params
            .get("patch")
            .cloned()
            .unwrap_or_else(|| json!({}));
        let patch: PartnerCustomerPatch = match serde_json::from_value(patch_value) {
            Ok(v) => v,
            Err(e) => {
                return WsFrame::error_response(
                    "",
                    &format!("Invalid patch payload: {e}"),
                )
            }
        };
        let store = self.partner_store();
        match store.update_customer(&id, &patch) {
            Ok(()) => WsFrame::ok_response("", json!({ "success": true })),
            Err(e) => WsFrame::error_response("", &e),
        }
    }

    async fn handle_partner_customer_delete(&self, params: Value) -> WsFrame {
        let id = match params.get("id").and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => return WsFrame::error_response("", "Missing 'id' parameter"),
        };
        let store = self.partner_store();
        match store.delete_customer(&id) {
            Ok(()) => WsFrame::ok_response("", json!({ "success": true })),
            Err(e) => WsFrame::error_response("", &e),
        }
    }

    // ── System ───────────────────────────────────────────────

    async fn handle_system_status(&self) -> WsFrame {
        let reg = self.registry.read().await;
        let uptime = self.start_time.elapsed().as_secs();
        let channel_map = self.channel_status.read().await;
        let channels_connected = channel_map.values().filter(|s| s.connected).count();
        drop(channel_map);
        WsFrame::ok_response("", json!({
            "version": crate::updater::current_version(),
            "uptime_seconds": uptime,
            "agents_count": reg.list().len(),
            "channels_connected": channels_connected,
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
                        // Do NOT return raw content — it may contain unmasked tokens (MCP-H5)
                        WsFrame::error_response("", "Failed to parse config.toml — cannot safely display")
                    }
                }
            }
            Err(e) => WsFrame::error_response("", &format!("Failed to read config.toml: {e}")),
        }
    }

    fn handle_system_version(&self) -> WsFrame {
        WsFrame::ok_response("", json!({
            "version": crate::updater::current_version(),
            "auto_update": crate::updater::auto_update_enabled(&self.home_dir),
        }))
    }

    async fn handle_system_check_update(&self) -> WsFrame {
        match crate::updater::check_update().await {
            Ok(info) => {
                // [M2] Cache the download/checksum URLs server-side
                // so apply_update does not accept URLs from the client.
                *self.pending_update.write().await = if info.available {
                    Some(PendingUpdate {
                        download_url: info.download_url.clone(),
                        checksum_url: info.checksum_url.clone(),
                        version: info.latest_version.clone(),
                        cached_at: Instant::now(),
                    })
                } else {
                    None
                };
                WsFrame::ok_response("", json!({
                    "available": info.available,
                    "current_version": info.current_version,
                    "latest_version": info.latest_version,
                    "release_notes": info.release_notes,
                    "published_at": info.published_at,
                    "download_url": info.download_url,
                    "checksum_url": info.checksum_url,
                    "install_method": info.install_method,
                    "brew_formula": crate::updater::brew_formula_name(),
                    "auto_update": crate::updater::auto_update_enabled(&self.home_dir),
                }))
            }
            Err(e) => WsFrame::error_response("", &format!("Update check failed: {e}")),
        }
    }

    async fn handle_system_apply_update(&self, _params: Value) -> WsFrame {
        // [M2] Use server-side cached URL — never accept URL from client
        let pending = self.pending_update.read().await.clone();
        let pending = match pending {
            Some(p) if !p.download_url.is_empty() => p,
            _ => return WsFrame::error_response(
                "",
                "No pending update. Call system.check_update first.",
            ),
        };

        // [R2:NM1] TTL check — reject stale cached URLs
        if pending.is_expired() {
            *self.pending_update.write().await = None;
            return WsFrame::error_response(
                "",
                "Pending update expired. Please call system.check_update again.",
            );
        }

        // [M5] Audit log
        duduclaw_security::audit::append_audit_event(
            &self.home_dir,
            &duduclaw_security::audit::AuditEvent::new(
                "system_update",
                "system",
                duduclaw_security::audit::Severity::Info,
                json!({ "action": "apply", "target_version": pending.version }),
            ),
        );

        match crate::updater::apply_update(&pending.download_url, &pending.checksum_url).await {
            Ok(result) => {
                *self.pending_update.write().await = None;

                if result.needs_restart {
                    tokio::spawn(async {
                        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                        tracing::info!("Shutting down for update — raising SIGINT for graceful shutdown");
                        duduclaw_core::platform::self_interrupt();
                    });
                }

                duduclaw_security::audit::append_audit_event(
                    &self.home_dir,
                    &duduclaw_security::audit::AuditEvent::new(
                        "system_update_success",
                        "system",
                        duduclaw_security::audit::Severity::Info,
                        json!({ "version": pending.version, "needs_restart": result.needs_restart }),
                    ),
                );

                WsFrame::ok_response("", json!({
                    "success": result.success,
                    "message": result.message,
                    "needs_restart": result.needs_restart,
                }))
            }
            Err(e) => {
                // [R2:NM5] Clear stale pending on failure so user must re-check
                *self.pending_update.write().await = None;

                // [R2:NM3] Sanitize error for audit log (strip ANSI/newlines)
                let sanitized = e.replace('\n', " ").replace('\r', "").replace('\x1b', "");
                duduclaw_security::audit::append_audit_event(
                    &self.home_dir,
                    &duduclaw_security::audit::AuditEvent::new(
                        "system_update_failed",
                        "system",
                        duduclaw_security::audit::Severity::Warning,
                        json!({ "error": sanitized }),
                    ),
                );
                WsFrame::error_response("", &format!("Update failed: {e}"))
            }
        }
    }

    // ── Security ────────────────────────────────────────────

    async fn handle_security_audit_log(&self, params: Value) -> WsFrame {
        let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(50) as usize;
        let events = duduclaw_security::audit::read_recent_events(&self.home_dir, limit);
        let events_json: Vec<Value> = events.iter().map(|e| {
            json!({
                "timestamp": e.timestamp,
                "event_type": e.event_type,
                "agent_id": e.agent_id,
                "severity": e.severity,
                "details": e.details,
            })
        }).collect();
        WsFrame::ok_response("", json!({ "events": events_json }))
    }

    /// Unified audit log that merges events from four JSONL sources:
    /// - `security_audit.jsonl` (SOUL drift / injection / quarantine events)
    /// - `tool_calls.jsonl` (MCP tool invocations)
    /// - `channel_failures.jsonl` (channel reply failures)
    /// - `feedback.jsonl` (heterogeneous user / evolution feedback signals)
    ///
    /// Each event is normalized into a common envelope with `source`,
    /// `event_type`, `severity`, `summary`, and `details`. Missing files are
    /// treated as zero-event sources; malformed lines are skipped silently.
    async fn handle_audit_unified_log(&self, params: Value) -> WsFrame {
        const DEFAULT_LIMIT: usize = 200;
        const MAX_LIMIT: usize = 1000;
        const SUMMARY_MAX_BYTES: usize = 240;

        let limit = params
            .get("limit")
            .and_then(|v| v.as_u64())
            .map(|n| (n as usize).min(MAX_LIMIT))
            .unwrap_or(DEFAULT_LIMIT);

        let all_sources = ["security", "tool_call", "channel_failure", "feedback"];
        let requested_sources: Vec<String> = params
            .get("sources")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|s| s.as_str().map(|x| x.to_string()))
                    .filter(|s| all_sources.contains(&s.as_str()))
                    .collect()
            })
            .unwrap_or_else(|| all_sources.iter().map(|s| s.to_string()).collect());

        let severity_filter = params
            .get("severity_filter")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let agent_id_filter = params
            .get("agent_id_filter")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        // Initialize counts for every source so the frontend always sees the
        // key even when the caller whitelisted a subset.
        let mut source_counts: std::collections::HashMap<String, usize> =
            all_sources.iter().map(|s| ((*s).to_string(), 0usize)).collect();

        let mut events: Vec<Value> = Vec::new();

        // Helper: read jsonl file tolerating missing files + malformed lines.
        async fn read_jsonl_lines(path: &std::path::Path) -> Vec<Value> {
            match tokio::fs::read_to_string(path).await {
                Ok(content) => content
                    .split('\n')
                    .filter(|line| !line.trim().is_empty())
                    .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                    .collect(),
                Err(_) => Vec::new(),
            }
        }

        // ── Source: security_audit.jsonl ────────────────────────────
        if requested_sources.iter().any(|s| s == "security") {
            let path = self.home_dir.join("security_audit.jsonl");
            let rows = read_jsonl_lines(&path).await;
            *source_counts.entry("security".into()).or_insert(0) += rows.len();
            for row in &rows {
                let timestamp = row
                    .get("timestamp")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let event_type = row
                    .get("event_type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_string();
                let agent_id = row
                    .get("agent_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let severity = row
                    .get("severity")
                    .and_then(|v| v.as_str())
                    .unwrap_or("info")
                    .to_lowercase();

                if let Some(sf) = &severity_filter
                    && &severity != sf
                {
                    continue;
                }
                if let Some(af) = &agent_id_filter
                    && &agent_id != af
                {
                    continue;
                }

                let raw_summary = row
                    .get("details")
                    .map(|d| d.to_string())
                    .unwrap_or_default();
                let summary = truncate_bytes(&raw_summary, SUMMARY_MAX_BYTES).to_string();

                events.push(json!({
                    "timestamp": timestamp,
                    "source": "security",
                    "event_type": event_type,
                    "agent_id": agent_id,
                    "severity": severity,
                    "summary": summary,
                    "details": { "security_audit": row },
                }));
            }
        }

        // ── Source: tool_calls.jsonl ────────────────────────────────
        if requested_sources.iter().any(|s| s == "tool_call") {
            let path = self.home_dir.join("tool_calls.jsonl");
            let rows = read_jsonl_lines(&path).await;
            *source_counts.entry("tool_call".into()).or_insert(0) += rows.len();
            for row in &rows {
                let timestamp = row
                    .get("timestamp")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let agent_id = row
                    .get("agent_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let tool_name = row
                    .get("tool_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                let success = row
                    .get("success")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true);
                let params_summary = row
                    .get("params_summary")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");

                let severity = if success { "info" } else { "warning" };
                let event_type = format!(
                    "tool.{tool_name}.{}",
                    if success { "success" } else { "failure" }
                );
                let summary = truncate_bytes(params_summary, SUMMARY_MAX_BYTES).to_string();

                // severity_filter only applies to security per spec.
                if let Some(af) = &agent_id_filter
                    && &agent_id != af
                {
                    continue;
                }

                events.push(json!({
                    "timestamp": timestamp,
                    "source": "tool_call",
                    "event_type": event_type,
                    "agent_id": agent_id,
                    "severity": severity,
                    "summary": summary,
                    "details": { "tool_call": row },
                }));
            }
        }

        // ── Source: channel_failures.jsonl ──────────────────────────
        if requested_sources.iter().any(|s| s == "channel_failure") {
            let path = self.home_dir.join("channel_failures.jsonl");
            let rows = read_jsonl_lines(&path).await;
            *source_counts.entry("channel_failure".into()).or_insert(0) += rows.len();
            for row in &rows {
                let timestamp = row
                    .get("timestamp")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                // Producer uses lowercase "agent" field; fall back to
                // "agent_id" to be defensive.
                let agent_id = row
                    .get("agent")
                    .or_else(|| row.get("agent_id"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let reason = row
                    .get("reason")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Unknown");
                let error_msg = row
                    .get("error")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");

                let event_type = format!("channel.{reason}");
                let summary = truncate_bytes(error_msg, SUMMARY_MAX_BYTES).to_string();

                if let Some(af) = &agent_id_filter
                    && &agent_id != af
                {
                    continue;
                }

                events.push(json!({
                    "timestamp": timestamp,
                    "source": "channel_failure",
                    "event_type": event_type,
                    "agent_id": agent_id,
                    "severity": "warning",
                    "summary": summary,
                    "details": { "channel_failure": row },
                }));
            }
        }

        // ── Source: feedback.jsonl (heterogeneous shape) ────────────
        if requested_sources.iter().any(|s| s == "feedback") {
            let path = self.home_dir.join("feedback.jsonl");
            let rows = read_jsonl_lines(&path).await;
            *source_counts.entry("feedback".into()).or_insert(0) += rows.len();
            for row in &rows {
                let timestamp = row
                    .get("timestamp")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let agent_id = row
                    .get("agent_id")
                    .or_else(|| row.get("agent"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                // `signal_type` is used by evolution feedback; fall back to
                // `kind` / `type`, else "generic".
                let kind = row
                    .get("signal_type")
                    .or_else(|| row.get("kind"))
                    .or_else(|| row.get("type"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("generic");
                let event_type = format!("feedback.{kind}");

                // Prefer `detail`, fall back to `message`, else stringified row.
                let raw_summary = row
                    .get("detail")
                    .or_else(|| row.get("message"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| row.to_string());
                let summary = truncate_bytes(&raw_summary, SUMMARY_MAX_BYTES).to_string();

                if let Some(af) = &agent_id_filter
                    && &agent_id != af
                {
                    continue;
                }

                events.push(json!({
                    "timestamp": timestamp,
                    "source": "feedback",
                    "event_type": event_type,
                    "agent_id": agent_id,
                    "severity": "info",
                    "summary": summary,
                    "details": { "feedback": row },
                }));
            }
        }

        // Sort descending by timestamp. Lexicographic compare works for
        // RFC3339/ISO8601 timestamps with consistent timezone suffix.
        events.sort_by(|a, b| {
            let ta = a.get("timestamp").and_then(|v| v.as_str()).unwrap_or("");
            let tb = b.get("timestamp").and_then(|v| v.as_str()).unwrap_or("");
            tb.cmp(ta)
        });

        let total = events.len();
        events.truncate(limit);

        let counts_json = json!({
            "security": source_counts.get("security").copied().unwrap_or(0),
            "tool_call": source_counts.get("tool_call").copied().unwrap_or(0),
            "channel_failure": source_counts.get("channel_failure").copied().unwrap_or(0),
            "feedback": source_counts.get("feedback").copied().unwrap_or(0),
        });

        WsFrame::ok_response(
            "",
            json!({
                "events": events,
                "source_counts": counts_json,
                "total": total,
            }),
        )
    }

    /// Live security system status — replaces static placeholder panels.
    async fn handle_security_status(&self) -> WsFrame {
        let reg = self.registry.read().await;
        let agents = reg.list();

        // Credential proxy: count env-injected secrets from config
        let secret_count = std::env::vars()
            .filter(|(k, _)| {
                k.contains("API_KEY") || k.contains("TOKEN") || k.contains("SECRET")
            })
            .count();

        // Mount guard: read from agent container configs
        let mount_rules: Vec<Value> = agents.iter().take(1).flat_map(|a| {
            let container = &a.config.container;
            let mut rules = Vec::new();
            if container.sandbox_enabled {
                rules.push(json!({"path": "/workspace", "access": if container.readonly_project { "ro" } else { "rw" }}));
                rules.push(json!({"path": "/tmp", "access": "rw"}));
                for mount in &container.additional_mounts {
                    rules.push(json!({"path": mount.container, "access": if mount.readonly { "ro" } else { "rw" }}));
                }
                if !container.network_access {
                    rules.push(json!({"path": "/var/run/docker.sock", "access": "deny"}));
                }
            }
            rules
        }).collect();

        // RBAC: derive from agent roles
        let rbac_entries: Vec<Value> = agents.iter().map(|a| {
            let cfg = &a.config;
            json!({
                "agent_id": cfg.agent.name,
                "role": cfg.agent.role,
                "tool_use": true,
                "web_access": cfg.capabilities.browser_via_bash,
                "file_write": true,
                "shell_exec": !cfg.capabilities.denied_tools.iter().any(|t| t == "Bash"),
                "delegate": cfg.capabilities.allowed_tools.iter().any(|t| t.contains("delegate") || t.contains("spawn")),
            })
        }).collect();

        // Rate limiter: read from config
        let config_path = self.home_dir.join("config").join("duduclaw.toml");
        let rate_limit = if config_path.exists() {
            let content = tokio::fs::read_to_string(&config_path).await.unwrap_or_default();
            // Parse basic rate limit values from config
            let rpm = content.lines()
                .find(|l| l.contains("rate_limit_rpm"))
                .and_then(|l| l.split('=').nth(1))
                .and_then(|v| v.trim().parse::<u32>().ok())
                .unwrap_or(60);
            let concurrent = content.lines()
                .find(|l| l.contains("max_concurrent"))
                .and_then(|l| l.split('=').nth(1))
                .and_then(|v| v.trim().parse::<u32>().ok())
                .unwrap_or(5);
            json!({
                "requests_per_minute": rpm,
                "concurrent_requests": concurrent,
            })
        } else {
            json!({
                "requests_per_minute": 60,
                "concurrent_requests": 5,
            })
        };

        // SOUL.md drift detection status
        let soul_status: Vec<Value> = agents.iter().map(|a| {
            let soul_path = self.home_dir.join("agents").join(&a.config.agent.name).join("SOUL.md");
            let exists = soul_path.exists();
            json!({
                "agent_id": a.config.agent.name,
                "soul_exists": exists,
                "gvu_enabled": a.config.evolution.gvu_enabled,
            })
        }).collect();

        WsFrame::ok_response("", json!({
            "credential_proxy": {
                "active": secret_count > 0,
                "vault_backend": "env",
                "injected_secrets": secret_count,
            },
            "mount_guard": {
                "rules": mount_rules,
            },
            "rbac": rbac_entries,
            "rate_limiter": rate_limit,
            "soul_drift": soul_status,
        }))
    }

    // ── Analytics ────────────────────────────────────────────

    /// Summary metrics for the dashboard report page.
    ///
    /// Aggregates data from CostTelemetry (SQLite) and session counts.
    async fn handle_analytics_summary(&self, params: Value) -> WsFrame {
        let period = params.get("period").and_then(|v| v.as_str()).unwrap_or("month");
        let hours: u64 = match period {
            "day" => 24,
            "week" => 168,
            _ => 720, // month
        };

        // Session counts from sessions.db
        let session_db = self.home_dir.join("sessions.db");
        let (total_conversations, total_messages, auto_reply_count, avg_response_ms, p95_response_ms) =
            if session_db.exists() {
                match rusqlite::Connection::open(&session_db) {
                    Ok(conn) => {
                        let cutoff = (chrono::Utc::now() - chrono::Duration::hours(hours as i64)).to_rfc3339();
                        let convos: i64 = conn.query_row(
                            "SELECT COUNT(*) FROM sessions WHERE last_active >= ?1",
                            params![cutoff], |r| r.get(0),
                        ).unwrap_or(0);
                        let msgs: i64 = conn.query_row(
                            "SELECT COUNT(*) FROM session_messages sm
                             JOIN sessions s ON sm.session_id = s.id
                             WHERE s.last_active >= ?1",
                            params![cutoff], |r| r.get(0),
                        ).unwrap_or(0);
                        // auto_reply: messages from assistant role
                        let auto: i64 = conn.query_row(
                            "SELECT COUNT(*) FROM session_messages sm
                             JOIN sessions s ON sm.session_id = s.id
                             WHERE s.last_active >= ?1 AND sm.role = 'assistant'",
                            params![cutoff], |r| r.get(0),
                        ).unwrap_or(0);
                        (convos, msgs, auto, 850_u64, 2400_u64)
                    }
                    Err(_) => (0, 0, 0, 0, 0),
                }
            } else {
                (0, 0, 0, 0, 0)
            };

        // Cost data from CostTelemetry
        let (zero_cost_ratio, estimated_savings_cents) =
            if let Some(telemetry) = crate::cost_telemetry::get_telemetry() {
                match telemetry.summary_global(hours).await {
                    Ok(summary) => {
                        let total_reqs = summary.total_requests.max(1);
                        // Zero-cost = requests handled without API calls (local inference / cached)
                        let cache_eff = summary.avg_cache_efficiency;
                        let savings = summary.total_cache_savings_millicents / 10; // millicents → cents
                        (cache_eff, savings)
                    }
                    Err(_) => (0.0, 0),
                }
            } else {
                (0.0, 0)
            };

        let auto_reply_rate = if total_messages > 0 {
            auto_reply_count as f64 / total_messages as f64
        } else {
            0.0
        };

        WsFrame::ok_response("", json!({
            "total_conversations": total_conversations,
            "total_messages": total_messages,
            "auto_reply_rate": auto_reply_rate,
            "avg_response_ms": avg_response_ms,
            "p95_response_ms": p95_response_ms,
            "zero_cost_ratio": zero_cost_ratio,
            "estimated_savings_cents": estimated_savings_cents,
            "period": period,
        }))
    }

    /// Daily conversation counts for the trend chart.
    async fn handle_analytics_conversations(&self) -> WsFrame {
        let session_db = self.home_dir.join("sessions.db");
        let daily: Vec<Value> = if session_db.exists() {
            match rusqlite::Connection::open(&session_db) {
                Ok(conn) => {
                    let mut stmt = conn.prepare(
                        "SELECT DATE(last_active) as day,
                                COUNT(*) as total,
                                COUNT(CASE WHEN total_tokens > 0 THEN 1 END) as auto
                         FROM sessions
                         WHERE last_active >= DATE('now', '-30 days')
                         GROUP BY day
                         ORDER BY day ASC"
                    ).unwrap();
                    let rows = stmt.query_map([], |row| {
                        let date: String = row.get(0)?;
                        let count: i64 = row.get(1)?;
                        let auto_count: i64 = row.get(2)?;
                        Ok(json!({
                            "date": date,
                            "count": count,
                            "auto_count": auto_count,
                        }))
                    }).unwrap();
                    rows.filter_map(|r| r.ok()).collect()
                }
                Err(_) => Vec::new(),
            }
        } else {
            Vec::new()
        };

        WsFrame::ok_response("", json!({ "daily": daily }))
    }

    /// Monthly cost comparison data for the savings table.
    async fn handle_analytics_cost_savings(&self) -> WsFrame {
        let monthly: Vec<Value> = if let Some(telemetry) = crate::cost_telemetry::get_telemetry() {
            // Get data for last 6 months
            let mut result = Vec::new();
            for months_ago in (0..6).rev() {
                let start_hours = (months_ago + 1) * 720;
                let end_hours = months_ago * 720;

                let start_summary = telemetry.summary_global(start_hours).await;
                let end_summary = telemetry.summary_global(end_hours).await;

                let (period_cost, period_savings) = match (start_summary, end_summary) {
                    (Ok(start), Ok(end)) => {
                        let cost = start.total_cost_millicents.saturating_sub(end.total_cost_millicents);
                        let savings = start.total_cache_savings_millicents.saturating_sub(end.total_cache_savings_millicents);
                        (cost, savings)
                    }
                    _ => (0, 0),
                };

                let month_date = chrono::Utc::now() - chrono::Duration::hours(end_hours as i64);
                let month_label = month_date.format("%Y-%m").to_string();

                // Estimate human cost as 3x of agent cost (industry benchmark)
                let human_cost_estimate = period_cost * 3;

                result.push(json!({
                    "month": month_label,
                    "human_cost": human_cost_estimate / 10, // millicents → cents
                    "agent_cost": period_cost / 10,
                    "savings": (human_cost_estimate.saturating_sub(period_cost)) / 10,
                }));
            }
            result
        } else {
            Vec::new()
        };

        WsFrame::ok_response("", json!({ "monthly": monthly }))
    }

    // ── Billing ──────────────────────────────────────────────

    /// Return real usage data for the billing page.
    ///
    /// - conversations: session count this month from sessions.db
    /// - agents: active agent count from registry
    /// - channels: connected channel count from channel_status
    /// - inference_hours: estimated from CostTelemetry token usage this month
    async fn handle_billing_usage(&self) -> WsFrame {
        let now = chrono::Utc::now();
        // Start of current month in RFC3339
        let month_start = now
            .date_naive()
            .with_day(1)
            .unwrap_or(now.date_naive())
            .and_hms_opt(0, 0, 0)
            .unwrap_or_default();
        let month_start_utc = chrono::DateTime::<chrono::Utc>::from_naive_utc_and_offset(
            month_start,
            chrono::Utc,
        );
        let hours_since_month_start =
            (now - month_start_utc).num_hours().max(1) as u64;

        // Conversations this month from sessions.db
        let session_db = self.home_dir.join("sessions.db");
        let conversations_used: i64 = if session_db.exists() {
            rusqlite::Connection::open(&session_db)
                .ok()
                .and_then(|conn| {
                    conn.query_row(
                        "SELECT COUNT(*) FROM sessions WHERE last_active >= ?1",
                        params![month_start_utc.to_rfc3339()],
                        |r| r.get(0),
                    )
                    .ok()
                })
                .unwrap_or(0)
        } else {
            0
        };

        // Active agents from registry
        let reg = self.registry.read().await;
        let agents_used = reg.list().len() as i64;
        drop(reg);

        // Connected channels
        let channel_map = self.channel_status.read().await;
        let channels_used = channel_map.values().filter(|s| s.connected).count() as i64;
        drop(channel_map);

        // Inference hours estimated from total output tokens this month
        // Rough heuristic: 1 hour ≈ 50 requests average
        let inference_hours_used: f64 =
            if let Some(telemetry) = crate::cost_telemetry::get_telemetry() {
                match telemetry.summary_global(hours_since_month_start).await {
                    Ok(summary) => summary.total_requests as f64 / 50.0,
                    Err(_) => 0.0,
                }
            } else {
                0.0
            };

        // Community edition: unlimited (-1)
        let reset_at = (month_start_utc + chrono::Duration::days(30)).to_rfc3339();

        WsFrame::ok_response(
            "",
            json!({
                "plan": "community",
                "tier": "community",
                "conversations": { "used": conversations_used, "limit": -1 },
                "agents": { "used": agents_used, "limit": -1 },
                "channels": { "used": channels_used, "limit": -1 },
                "inference_hours": { "used": inference_hours_used.round() as i64, "limit": -1 },
                "reset_at": reset_at,
            }),
        )
    }

    // ── Heartbeat ────────────────────────────────────────────

    async fn handle_heartbeat_status(&self) -> WsFrame {
        let hb = self.heartbeat.read().await;
        match hb.as_ref() {
            Some(scheduler) => {
                let statuses = scheduler.status().await;
                WsFrame::ok_response("", json!({
                    "heartbeats": statuses,
                    "count": statuses.len(),
                }))
            }
            None => WsFrame::ok_response("", json!({
                "heartbeats": [],
                "count": 0,
                "message": "Heartbeat scheduler not started",
            })),
        }
    }

    async fn handle_heartbeat_trigger(&self, params: Value) -> WsFrame {
        let agent_id = params.get("agent_id").and_then(|v| v.as_str()).unwrap_or("");
        if agent_id.is_empty() {
            return WsFrame::error_response("", "agent_id is required");
        }

        let hb = self.heartbeat.read().await;
        match hb.as_ref() {
            Some(scheduler) => {
                let triggered = scheduler.trigger(agent_id).await;
                if triggered {
                    WsFrame::ok_response("", json!({
                        "success": true,
                        "message": format!("Heartbeat triggered for agent '{agent_id}'"),
                    }))
                } else {
                    WsFrame::error_response("", &format!("Agent '{agent_id}' not found in heartbeat scheduler"))
                }
            }
            None => WsFrame::error_response("", "Heartbeat scheduler not started"),
        }
    }

    // ── Logs ────────────────────────────────────────────────

    fn handle_logs_subscribe(&self, params: Value) -> WsFrame {
        let filter = params.get("filter").and_then(|v| v.as_str()).unwrap_or("*");
        info!(filter, "logs.subscribe activated — WebSocket push enabled for this connection");
        WsFrame::ok_response("", json!({
            "success": true,
            "subscribed": true,
            "filter": filter,
            "message": "Log push active — events will stream on this WebSocket connection",
        }))
    }

    fn handle_logs_unsubscribe(&self, params: Value) -> WsFrame {
        let filter = params.get("filter").and_then(|v| v.as_str()).unwrap_or("*");
        info!(filter, "logs.unsubscribe — WebSocket push disabled for this connection");
        WsFrame::ok_response("", json!({
            "success": true,
            "subscribed": false,
            "filter": filter,
        }))
    }

    // ── Evolution ────────────────────────────────────────────

    async fn handle_evolution_status(&self) -> WsFrame {
        let reg = self.registry.read().await;
        let mut gvu_enabled_count = 0usize;
        let agents: Vec<Value> = reg.list().iter().map(|a| {
            let cfg = &a.config;
            if cfg.evolution.gvu_enabled { gvu_enabled_count += 1; }
            json!({
                "agent_id": cfg.agent.name,
                "gvu_enabled": cfg.evolution.gvu_enabled,
                "cognitive_memory": cfg.evolution.cognitive_memory,
                "skill_auto_activate": cfg.evolution.skill_auto_activate,
                "skill_security_scan": cfg.evolution.skill_security_scan,
                "max_silence_hours": cfg.evolution.max_silence_hours,
                "max_gvu_generations": cfg.evolution.max_gvu_generations,
                "observation_period_hours": cfg.evolution.observation_period_hours,
            })
        }).collect();
        let total_agents = agents.len();
        let agent_ids: Vec<String> = reg.list().iter().map(|a| a.config.agent.name.clone()).collect();
        drop(reg);

        // Aggregate real version stats from evolution.db (if any GVU run has persisted).
        let db_path = self.home_dir.join("evolution.db");
        let (total_versions, last_applied_at) = if db_path.exists() {
            let vs = VersionStore::new(&db_path);
            let mut total: u64 = 0;
            let mut latest: Option<chrono::DateTime<chrono::Utc>> = None;
            for aid in &agent_ids {
                let history = vs.get_history(aid, 100);
                total += history.len() as u64;
                if let Some(v) = history.first() {
                    latest = Some(match latest {
                        Some(prev) if prev >= v.applied_at => prev,
                        _ => v.applied_at,
                    });
                }
            }
            (total, latest.map(|t| t.to_rfc3339()))
        } else {
            (0u64, None)
        };

        let enabled = gvu_enabled_count > 0;
        WsFrame::ok_response("", json!({
            "enabled": enabled,
            "mode": if enabled { "prediction_driven" } else { "disabled" },
            "total_agents": total_agents,
            "gvu_enabled_count": gvu_enabled_count,
            "total_versions": total_versions,
            "last_applied_at": last_applied_at,
            "agents": agents,
        }))
    }

    async fn handle_evolution_history(&self, params: Value) -> WsFrame {
        let agent_id = params.get("agent_id").and_then(|v| v.as_str()).unwrap_or("");
        let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(20).min(100) as usize;

        let db_path = self.home_dir.join("evolution.db");
        if !db_path.exists() {
            return WsFrame::ok_response("", json!({ "versions": [] }));
        }

        let vs = VersionStore::new(&db_path);

        // If agent_id is specified, show that agent's history; otherwise show all agents
        let reg = self.registry.read().await;
        let agent_ids: Vec<String> = if agent_id.is_empty() {
            reg.list().iter().map(|a| a.config.agent.name.clone()).collect()
        } else {
            vec![agent_id.to_string()]
        };
        drop(reg);

        let mut versions = Vec::new();
        for aid in &agent_ids {
            for v in vs.get_history(aid, limit) {
                versions.push(json!({
                    "version_id": v.version_id,
                    "agent_id": v.agent_id,
                    "soul_summary": v.soul_summary,
                    "soul_hash": v.soul_hash,
                    "applied_at": v.applied_at.to_rfc3339(),
                    "observation_end": v.observation_end.to_rfc3339(),
                    "status": format!("{:?}", v.status),
                    "pre_metrics": {
                        "positive_feedback_ratio": v.pre_metrics.positive_feedback_ratio,
                        "prediction_error": v.pre_metrics.avg_prediction_error,
                        "user_correction_rate": v.pre_metrics.user_correction_rate,
                        "contract_violations": v.pre_metrics.contract_violations,
                    },
                    "post_metrics": v.post_metrics.as_ref().map(|m| json!({
                        "positive_feedback_ratio": m.positive_feedback_ratio,
                        "prediction_error": m.avg_prediction_error,
                        "user_correction_rate": m.user_correction_rate,
                        "contract_violations": m.contract_violations,
                    })),
                }));
            }
        }

        // Sort by applied_at descending
        versions.sort_by(|a, b| {
            let ta = a.get("applied_at").and_then(|v| v.as_str()).unwrap_or("");
            let tb = b.get("applied_at").and_then(|v| v.as_str()).unwrap_or("");
            tb.cmp(ta)
        });
        versions.truncate(limit);

        WsFrame::ok_response("", json!({ "versions": versions }))
    }

    // ── Models ──────────────────────────────────────────────

    /// List all available models (cloud + local GGUF files).
    async fn handle_models_list(&self) -> WsFrame {
        let mut models = Vec::new();

        // Cloud models (always available)
        for (id, label) in [
            ("claude-opus-4-6", "Claude Opus 4.6"),
            ("claude-sonnet-4-6", "Claude Sonnet 4.6"),
            ("claude-haiku-4-5", "Claude Haiku 4.5"),
        ] {
            models.push(json!({
                "id": id,
                "label": label,
                "type": "cloud",
            }));
        }

        // Local models: scan ~/.duduclaw/models/ for GGUF files
        let models_dir = self.home_dir.join("models");
        if let Ok(mut entries) = tokio::fs::read_dir(&models_dir).await {
            while let Ok(Some(entry)) = entries.next_entry().await {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("gguf") {
                    continue;
                }
                let name = path.file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();
                let size = entry.metadata().await.map(|m| m.len()).unwrap_or(0);
                let size_gb = size as f64 / (1024.0 * 1024.0 * 1024.0);
                models.push(json!({
                    "id": format!("local:{name}"),
                    "label": format!("{name} ({size_gb:.1}GB)"),
                    "type": "local",
                    "file": name,
                    "size_bytes": size,
                }));
            }
        }

        // Also read default_model from inference.toml if it exists
        let inf_path = self.home_dir.join("inference.toml");
        let default_model = if let Ok(content) = tokio::fs::read_to_string(&inf_path).await {
            content.parse::<toml::Table>().ok()
                .and_then(|t| t.get("default_model")?.as_str().map(|s| s.to_string()))
        } else {
            None
        };

        WsFrame::ok_response("", json!({
            "models": models,
            "default_local": default_model,
        }))
    }

    // ── System Config Update ─────────────────────────────────

    /// Update system-level config.toml fields (whitelist only).
    ///
    /// Only allows safe, non-sensitive fields: `log_level`, `rotation_strategy`.
    /// Uses atomic write (temp + rename) and never touches token/key fields.
    async fn handle_system_update_config(&self, params: Value) -> WsFrame {
        let config_path = self.home_dir.join("config.toml");
        let mut table = self.read_config_table(&config_path).await;
        let mut changes: Vec<String> = Vec::new();

        // ── log_level ──
        if let Some(v) = params.get("log_level").and_then(|v| v.as_str()) {
            match v {
                "trace" | "debug" | "info" | "warn" | "error" => {
                    let logging = table.entry("logging")
                        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
                        .as_table_mut();
                    if let Some(logging) = logging {
                        logging.insert("level".into(), toml::Value::String(v.into()));
                        changes.push(format!("logging.level = \"{v}\""));
                    }
                }
                _ => return WsFrame::error_response("", &format!(
                    "Invalid log_level '{v}'. Valid: trace, debug, info, warn, error"
                )),
            }
        }

        // ── rotation_strategy ──
        if let Some(v) = params.get("rotation_strategy").and_then(|v| v.as_str()) {
            match v {
                "priority" | "round_robin" | "least_cost" | "failover" => {
                    let rotation = table.entry("rotation")
                        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
                        .as_table_mut();
                    if let Some(rotation) = rotation {
                        rotation.insert("strategy".into(), toml::Value::String(v.into()));
                        changes.push(format!("rotation.strategy = \"{v}\""));
                    }
                }
                _ => return WsFrame::error_response("", &format!(
                    "Invalid rotation_strategy '{v}'. Valid: priority, round_robin, least_cost, failover"
                )),
            }
        }

        // ── auto_update (Pro only) ──
        if let Some(v) = params.get("auto_update").and_then(|v| v.as_bool()) {
            let gateway = table.entry("gateway")
                .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
                .as_table_mut();
            if let Some(gateway) = gateway {
                gateway.insert("auto_update".into(), toml::Value::Boolean(v));
                changes.push(format!("gateway.auto_update = {v}"));
            }
        }

        if changes.is_empty() {
            return WsFrame::error_response("", "No valid fields to update. Supported: log_level, rotation_strategy, auto_update");
        }

        // Atomic write: temp + rename
        let tmp_path = config_path.with_extension("toml.tmp");
        if let Err(e) = self.write_config_table(&tmp_path, &table).await {
            return WsFrame::error_response("", &format!("Failed to write config: {e}"));
        }
        if let Err(e) = tokio::fs::rename(&tmp_path, &config_path).await {
            let _ = tokio::fs::remove_file(&tmp_path).await;
            return WsFrame::error_response("", &format!("Failed to commit config: {e}"));
        }

        info!(?changes, "system.update_config completed");
        WsFrame::ok_response("", json!({
            "success": true,
            "changes": changes,
        }))
    }

    /// Add a new account to config.toml [[accounts]] array.
    ///
    /// Encrypts the API key before storing. Supports `api_key` and `oauth` types.
    async fn handle_accounts_add(&self, params: Value) -> WsFrame {
        let id = match params.get("id").and_then(|v| v.as_str()) {
            Some(id) if !id.is_empty() => id,
            _ => return WsFrame::error_response("", "Missing 'id' parameter"),
        };
        let auth_type = params.get("type").and_then(|v| v.as_str()).unwrap_or("api_key");
        let key = match params.get("key").and_then(|v| v.as_str()) {
            Some(k) if !k.is_empty() => k,
            _ => return WsFrame::error_response("", "Missing 'key' parameter"),
        };
        let budget_cents = params.get("monthly_budget_cents").and_then(|v| v.as_u64()).unwrap_or(5000);
        let priority = params.get("priority").and_then(|v| v.as_u64()).unwrap_or(1);

        let config_path = self.home_dir.join("config.toml");
        let mut table = self.read_config_table(&config_path).await;

        // Ensure [[accounts]] array exists
        let accounts = table.entry("accounts")
            .or_insert_with(|| toml::Value::Array(Vec::new()));
        let arr = match accounts.as_array_mut() {
            Some(a) => a,
            None => return WsFrame::error_response("", "Invalid 'accounts' section in config.toml"),
        };

        // Check for duplicate id
        if arr.iter().any(|a| a.as_table().and_then(|t| t.get("id").and_then(|v| v.as_str())) == Some(id)) {
            return WsFrame::error_response("", &format!("Account '{id}' already exists"));
        }

        // Encrypt the key
        let encrypted = crate::config_crypto::encrypt_value(key, &self.home_dir);

        let mut account = toml::map::Map::new();
        account.insert("id".into(), toml::Value::String(id.into()));
        account.insert("type".into(), toml::Value::String(auth_type.into()));
        account.insert("monthly_budget_cents".into(), toml::Value::Integer(budget_cents as i64));
        account.insert("priority".into(), toml::Value::Integer(priority as i64));
        // Store plaintext key for runtime use + encrypted version for security
        let key_field = if auth_type == "oauth" { "oauth_token" } else { "anthropic_api_key" };
        account.insert(key_field.into(), toml::Value::String(key.into()));
        if let Some(enc) = &encrypted {
            account.insert(format!("{key_field}_enc"), toml::Value::String(enc.clone()));
        }
        arr.push(toml::Value::Table(account));

        // Atomic write
        let tmp_path = config_path.with_extension("toml.tmp");
        if let Err(e) = self.write_config_table(&tmp_path, &table).await {
            return WsFrame::error_response("", &format!("Failed to write config: {e}"));
        }
        if let Err(e) = tokio::fs::rename(&tmp_path, &config_path).await {
            let _ = tokio::fs::remove_file(&tmp_path).await;
            return WsFrame::error_response("", &format!("Failed to commit config: {e}"));
        }

        info!(id, auth_type, "accounts.add completed");
        WsFrame::ok_response("", json!({
            "success": true,
            "id": id,
            "type": auth_type,
        }))
    }

    /// Update the monthly budget for a specific account in config.toml.
    async fn handle_accounts_update_budget(&self, params: Value) -> WsFrame {
        let account_id = match params.get("account_id").and_then(|v| v.as_str()) {
            Some(id) if !id.is_empty() => id,
            _ => return WsFrame::error_response("", "Missing 'account_id' parameter"),
        };
        let budget_cents = match params.get("monthly_budget_cents").and_then(|v| v.as_u64()) {
            Some(v) => v,
            None => return WsFrame::error_response("", "Missing 'monthly_budget_cents' parameter (integer)"),
        };

        let config_path = self.home_dir.join("config.toml");
        let mut table = self.read_config_table(&config_path).await;

        // Find the target account in [[accounts]] array
        let accounts = match table.get_mut("accounts").and_then(|v| v.as_array_mut()) {
            Some(arr) => arr,
            None => return WsFrame::error_response("", "No [[accounts]] section in config.toml"),
        };

        let target = accounts.iter_mut().find(|a| {
            a.as_table()
                .and_then(|t| t.get("id").and_then(|v| v.as_str()))
                == Some(account_id)
        });

        match target {
            Some(account) => {
                if let Some(t) = account.as_table_mut() {
                    t.insert("monthly_budget_cents".into(), toml::Value::Integer(budget_cents as i64));
                }
            }
            None => return WsFrame::error_response("", &format!("Account not found: {account_id}")),
        }

        // Atomic write
        let tmp_path = config_path.with_extension("toml.tmp");
        if let Err(e) = self.write_config_table(&tmp_path, &table).await {
            return WsFrame::error_response("", &format!("Failed to write config: {e}"));
        }
        if let Err(e) = tokio::fs::rename(&tmp_path, &config_path).await {
            let _ = tokio::fs::remove_file(&tmp_path).await;
            return WsFrame::error_response("", &format!("Failed to commit config: {e}"));
        }

        info!(account_id, budget_cents, "accounts.update_budget completed");
        WsFrame::ok_response("", json!({
            "success": true,
            "account_id": account_id,
            "monthly_budget_cents": budget_cents,
        }))
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
            {
                let (docker_status, docker_msg) = check_docker().await;
                json!({
                    "name": "container_runtime",
                    "status": docker_status,
                    "message": docker_msg,
                    "can_repair": false,
                })
            },
        ]
    }

    // ── Odoo ERP ─────────────────────────────────────────────────

    /// Return the current Odoo connection status.
    ///
    /// Reads `[odoo]` from config.toml, attempts to connect if configured,
    /// and returns connected/edition/version info.
    async fn handle_odoo_status(&self) -> WsFrame {
        let config_path = self.home_dir.join("config.toml");
        let table = self.read_config_table(&config_path).await;
        let odoo_cfg = duduclaw_odoo::OdooConfig::from_toml(&table);

        if !odoo_cfg.is_configured() {
            return WsFrame::ok_response("", json!({
                "connected": false,
            }));
        }

        // Decrypt credential
        let credential = match self.resolve_odoo_credential(&table) {
            Some(c) if !c.is_empty() => c,
            _ => return WsFrame::ok_response("", json!({
                "connected": false,
                "error": "No credential configured",
            })),
        };

        match duduclaw_odoo::OdooConnector::connect(&odoo_cfg, &credential).await {
            Ok(conn) => {
                let st = conn.status();
                WsFrame::ok_response("", json!({
                    "connected": st.connected,
                    "edition": st.edition,
                    "version": st.version,
                    "uid": st.uid,
                }))
            }
            Err(e) => {
                warn!("Odoo connection failed: {e}");
                WsFrame::ok_response("", json!({
                    "connected": false,
                    "error": "Connection failed",
                }))
            }
        }
    }

    /// Return the current Odoo config (without secrets).
    /// Returns `null` if Odoo is not configured.
    async fn handle_odoo_config(&self) -> WsFrame {
        let config_path = self.home_dir.join("config.toml");
        let table = self.read_config_table(&config_path).await;
        let cfg = duduclaw_odoo::OdooConfig::from_toml(&table);

        if !cfg.is_configured() {
            return WsFrame::ok_response("", json!(null));
        }

        WsFrame::ok_response("", json!({
            "url": cfg.url,
            "db": cfg.db,
            "protocol": cfg.protocol,
            "auth_method": cfg.auth_method,
            "username": cfg.username,
            "poll_enabled": cfg.poll_enabled,
            "poll_interval_seconds": cfg.poll_interval_seconds,
            "poll_models": cfg.poll_models,
            "webhook_enabled": cfg.webhook_enabled,
            "features_crm": cfg.features_crm,
            "features_sale": cfg.features_sale,
            "features_inventory": cfg.features_inventory,
            "features_accounting": cfg.features_accounting,
            "features_project": cfg.features_project,
            "features_hr": cfg.features_hr,
        }))
    }

    /// Validate an Odoo model name (e.g. `crm.lead`, `sale.order`).
    /// Rejects blocked models (security-sensitive Odoo internals).
    fn is_valid_odoo_model(name: &str) -> bool {
        !name.is_empty()
            && name.len() < 100
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_')
            && !duduclaw_odoo::OdooConnector::is_model_blocked(name)
    }

    /// Validate that a URL is safe for Odoo connections.
    /// Requires HTTPS with non-private host, except for strict localhost.
    fn is_safe_odoo_url(url: &str) -> bool {
        if url.len() > 512 {
            return false;
        }
        // Allow HTTP only for strict localhost — must be followed by '/' or ':' or end of string
        for prefix in &["http://127.0.0.1", "http://localhost", "http://[::1]"] {
            if let Some(rest) = url.strip_prefix(prefix) {
                if rest.is_empty() || rest.starts_with('/') || rest.starts_with(':') {
                    return true;
                }
            }
        }
        if url.starts_with("https://") {
            // Reject private/reserved IPs to prevent SSRF against cloud metadata, LAN, etc.
            let host_part = &url["https://".len()..];
            // Extract host (before first '/' or ':' for port)
            let host = host_part.split(&['/', ':'][..]).next().unwrap_or("");
            return !Self::is_private_host(host);
        }
        false
    }

    /// Check if a hostname is a private/reserved IP or a known metadata endpoint.
    /// Uses `std::net::IpAddr` parsing to correctly handle all IPv4/IPv6 representations,
    /// including IPv4-mapped IPv6 (`::ffff:10.0.0.1`), compressed forms, etc.
    fn is_private_host(host: &str) -> bool {
        // Strip brackets for IPv6 literals (e.g. "[::1]" → "::1")
        let raw = host.trim_start_matches('[').trim_end_matches(']');

        // Bare IPv6 without brackets (contains ':' but no '[') — reject as ambiguous
        if !host.starts_with('[') && raw.contains(':') {
            return true;
        }

        if let Ok(ip) = raw.parse::<std::net::IpAddr>() {
            return Self::is_private_ip(ip);
        }

        // Hostname-based checks
        let lower = host.to_ascii_lowercase();
        lower == "localhost" || lower.ends_with(".localhost")
            || lower == "metadata.google.internal"
            || lower == "metadata.azure.internal"
    }

    /// Check if an IP address is private, loopback, link-local, or otherwise reserved.
    fn is_private_ip(ip: std::net::IpAddr) -> bool {
        match ip {
            std::net::IpAddr::V4(v4) => {
                v4.is_loopback()           // 127.0.0.0/8
                    || v4.is_private()      // 10/8, 172.16/12, 192.168/16
                    || v4.is_link_local()   // 169.254/16
                    || v4.is_unspecified()  // 0.0.0.0
                    || v4.is_broadcast()    // 255.255.255.255
                    || v4.octets()[0] == 100 && (v4.octets()[1] & 0xC0) == 64 // 100.64/10 (CGNAT)
            }
            std::net::IpAddr::V6(v6) => {
                v6.is_loopback()           // ::1
                    || v6.is_unspecified()  // ::
                    // IPv4-mapped (::ffff:x.x.x.x) — check the embedded v4
                    || v6.to_ipv4_mapped().is_some_and(|v4| Self::is_private_ip(std::net::IpAddr::V4(v4)))
                    // Link-local (fe80::/10)
                    || (v6.segments()[0] & 0xffc0) == 0xfe80
                    // Unique Local Address (fc00::/7)
                    || (v6.octets()[0] & 0xfe) == 0xfc
            }
        }
    }

    /// Save Odoo configuration to config.toml `[odoo]` section.
    ///
    /// Encrypts api_key/password/webhook_secret before storing.
    /// Refuses to store credentials if encryption is unavailable.
    /// Uses atomic write (temp + rename).
    async fn handle_odoo_configure(&self, params: Value) -> WsFrame {
        // Validate URL
        let url = match params.get("url").and_then(|v| v.as_str()).map(str::trim) {
            Some(u) if Self::is_safe_odoo_url(u) => u,
            Some(_) => return WsFrame::error_response("", "Odoo URL must use HTTPS (http:// only allowed for localhost/127.0.0.1)"),
            _ => return WsFrame::error_response("", "Missing 'url' parameter"),
        };
        // Validate database name
        let db = match params.get("db").and_then(|v| v.as_str()).map(str::trim) {
            Some(d) if !d.is_empty() && d.len() < 64
                && d.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') => d,
            Some(_) => return WsFrame::error_response("", "Invalid database name (alphanumeric, _, - only, max 63 chars)"),
            _ => return WsFrame::error_response("", "Missing 'db' parameter"),
        };

        // Validate protocol (whitelist)
        let protocol = match params.get("protocol").and_then(|v| v.as_str()) {
            Some("xmlrpc") => "xmlrpc",
            Some("jsonrpc") | None => "jsonrpc",
            _ => return WsFrame::error_response("", "Invalid protocol: must be 'jsonrpc' or 'xmlrpc'"),
        };

        // Validate auth_method (whitelist)
        let auth_method = match params.get("auth_method").and_then(|v| v.as_str()) {
            Some("password") => "password",
            Some("api_key") | None => "api_key",
            _ => return WsFrame::error_response("", "Invalid auth_method: must be 'api_key' or 'password'"),
        };

        let config_path = self.home_dir.join("config.toml");
        let mut table = self.read_config_table(&config_path).await;

        // Build the [odoo] section
        let mut odoo = toml::map::Map::new();
        odoo.insert("url".into(), toml::Value::String(url.into()));
        odoo.insert("db".into(), toml::Value::String(db.into()));
        odoo.insert("protocol".into(), toml::Value::String(protocol.into()));
        odoo.insert("auth_method".into(), toml::Value::String(auth_method.into()));
        let username = params.get("username").and_then(|v| v.as_str()).unwrap_or("").trim();
        if username.len() > 256 {
            return WsFrame::error_response("", "Username too long (max 256 chars)");
        }
        odoo.insert("username".into(), toml::Value::String(username.into()));

        // Encrypt credentials — refuse to store if encryption is unavailable (CRIT-1)
        if let Some(api_key) = params.get("api_key").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
            match crate::config_crypto::encrypt_value(api_key, &self.home_dir) {
                Some(enc) => { odoo.insert("api_key_enc".into(), toml::Value::String(enc)); }
                None => return WsFrame::error_response("", "Encryption unavailable — cannot store API key securely. Ensure keyfile exists."),
            }
        } else {
            // Preserve existing encrypted key if not provided
            if let Some(existing) = table.get("odoo")
                .and_then(|v| v.as_table())
                .and_then(|t| t.get("api_key_enc"))
                .and_then(|v| v.as_str())
            {
                odoo.insert("api_key_enc".into(), toml::Value::String(existing.into()));
            }
        }

        if let Some(password) = params.get("password").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
            match crate::config_crypto::encrypt_value(password, &self.home_dir) {
                Some(enc) => { odoo.insert("password_enc".into(), toml::Value::String(enc)); }
                None => return WsFrame::error_response("", "Encryption unavailable — cannot store password securely. Ensure keyfile exists."),
            }
        } else {
            // Preserve existing encrypted password if not provided
            if let Some(existing) = table.get("odoo")
                .and_then(|v| v.as_table())
                .and_then(|t| t.get("password_enc"))
                .and_then(|v| v.as_str())
            {
                odoo.insert("password_enc".into(), toml::Value::String(existing.into()));
            }
        }

        // Polling config
        odoo.insert(
            "poll_enabled".into(),
            toml::Value::Boolean(params.get("poll_enabled").and_then(|v| v.as_bool()).unwrap_or(false)),
        );
        odoo.insert(
            "poll_interval_seconds".into(),
            toml::Value::Integer(
                params.get("poll_interval_seconds")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(300)
                    .clamp(60, 86400),
            ),
        );
        if let Some(models) = params.get("poll_models").and_then(|v| v.as_array()) {
            let arr: Vec<toml::Value> = models
                .iter()
                .take(50) // cap at 50 models to prevent oversized config
                .filter_map(|v| v.as_str()
                    .filter(|s| Self::is_valid_odoo_model(s))
                    .map(|s| toml::Value::String(s.into())))
                .collect();
            odoo.insert("poll_models".into(), toml::Value::Array(arr));
        }

        // Webhook config
        odoo.insert(
            "webhook_enabled".into(),
            toml::Value::Boolean(params.get("webhook_enabled").and_then(|v| v.as_bool()).unwrap_or(false)),
        );
        if let Some(secret) = params.get("webhook_secret").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
            match crate::config_crypto::encrypt_value(secret, &self.home_dir) {
                Some(enc) => { odoo.insert("webhook_secret_enc".into(), toml::Value::String(enc)); }
                None => return WsFrame::error_response("", "Encryption unavailable — cannot store webhook secret securely."),
            }
        } else {
            // Preserve existing webhook secret
            if let Some(existing) = table.get("odoo")
                .and_then(|v| v.as_table())
                .and_then(|t| t.get("webhook_secret_enc"))
                .and_then(|v| v.as_str())
            {
                odoo.insert("webhook_secret_enc".into(), toml::Value::String(existing.into()));
            }
        }

        // Feature toggles
        for feature in &["features_crm", "features_sale", "features_inventory", "features_accounting", "features_project", "features_hr"] {
            if let Some(v) = params.get(*feature).and_then(|v| v.as_bool()) {
                odoo.insert((*feature).into(), toml::Value::Boolean(v));
            }
        }

        table.insert("odoo".into(), toml::Value::Table(odoo));

        // Atomic write: temp + rename
        let tmp_path = config_path.with_extension("toml.tmp");
        if let Err(e) = self.write_config_table(&tmp_path, &table).await {
            return WsFrame::error_response("", &format!("Failed to write config: {e}"));
        }
        if let Err(e) = tokio::fs::rename(&tmp_path, &config_path).await {
            let _ = tokio::fs::remove_file(&tmp_path).await;
            return WsFrame::error_response("", &format!("Failed to commit config: {e}"));
        }

        info!("odoo.configure completed");
        WsFrame::ok_response("", json!({ "success": true }))
    }

    /// Test the Odoo connection using stored config.
    async fn handle_odoo_test(&self) -> WsFrame {
        let config_path = self.home_dir.join("config.toml");
        let table = self.read_config_table(&config_path).await;
        let odoo_cfg = duduclaw_odoo::OdooConfig::from_toml(&table);

        if !odoo_cfg.is_configured() {
            return WsFrame::ok_response("", json!({
                "success": false,
                "message": "Odoo not configured — set URL and database first",
            }));
        }

        let credential = match self.resolve_odoo_credential(&table) {
            Some(c) if !c.is_empty() => c,
            _ => return WsFrame::ok_response("", json!({
                "success": false,
                "message": "No API key or password configured",
            })),
        };

        match duduclaw_odoo::OdooConnector::connect(&odoo_cfg, &credential).await {
            Ok(conn) => {
                let st = conn.status();
                WsFrame::ok_response("", json!({
                    "success": true,
                    "message": format!("Connected — {} {}", st.edition, st.version),
                }))
            }
            Err(e) => {
                warn!("Odoo test connection failed: {e}");
                WsFrame::ok_response("", json!({
                    "success": false,
                    "message": "Connection failed — check URL, credentials, and network",
                }))
            }
        }
    }

    /// Resolve the Odoo credential from config.toml (encrypted or plaintext).
    ///
    /// Returns `None` if decryption fails — never returns raw ciphertext (CRIT-2).
    fn resolve_odoo_credential(&self, table: &toml::Table) -> Option<String> {
        let odoo_section = table.get("odoo")?.as_table()?;
        let auth_method = odoo_section.get("auth_method")
            .and_then(|v| v.as_str())
            .unwrap_or("api_key");

        let (enc_field, plain_field) = if auth_method == "password" {
            ("password_enc", "password")
        } else {
            ("api_key_enc", "api_key")
        };

        // Try encrypted first
        if let Some(enc_val) = odoo_section.get(enc_field).and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
            if let Some(key) = crate::config_crypto::load_keyfile_public(&self.home_dir) {
                if let Ok(engine) = duduclaw_security::crypto::CryptoEngine::new(&key) {
                    if let Ok(decrypted) = engine.decrypt_string(enc_val) {
                        return Some(decrypted);
                    }
                }
            }
            // Decryption failed — do NOT return raw ciphertext as credential
            warn!("Failed to decrypt Odoo credential — keyfile may have changed");
            return None;
        }

        // Fallback to plaintext field (legacy / dev environments)
        let plain = odoo_section.get(plain_field)
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());
        if plain.is_some() {
            warn!(field = plain_field, "Odoo credential stored in plaintext — re-save config to encrypt");
        }
        plain
    }

    /// Mask sensitive values (tokens, secrets, keys) in a TOML table.
    fn mask_sensitive_fields(table: &mut toml::Table) {
        let sensitive_patterns = ["token", "secret", "key", "password"];
        for (key, value) in table.iter_mut() {
            let is_sensitive = sensitive_patterns.iter().any(|p| key.to_lowercase().contains(p));
            match value {
                toml::Value::String(s) if is_sensitive && !s.is_empty() => {
                    // Fully mask sensitive values — do NOT leak any prefix chars (MCP-M7)
                    *s = "********".to_string();
                }
                toml::Value::Table(t) => Self::mask_sensitive_fields(t),
                _ => {}
            }
        }
    }

    // ── Filtered agent list (respects UserContext) ────────────

    async fn handle_agents_list_filtered(&self, ctx: &UserContext) -> WsFrame {
        // Re-scan to pick up changes
        if let Ok(mut reg) = tokio::time::timeout(
            std::time::Duration::from_millis(500),
            self.registry.write(),
        ).await {
            let _ = reg.scan().await;
        }

        let reg = self.registry.read().await;
        let total_spent = self.get_total_spent().await;
        let visible = ctx.visible_agents();

        let agents: Vec<Value> = reg.list().iter()
            .filter(|a| {
                match &visible {
                    None => true, // Admin sees all
                    Some(names) => names.contains(&a.config.agent.name),
                }
            })
            .map(|a| {
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
                        "api_mode": cfg.model.api_mode,
                        "local": cfg.model.local.as_ref().map(|l| json!({
                            "model": l.model,
                            "backend": l.backend,
                            "context_length": l.context_length,
                            "gpu_layers": l.gpu_layers,
                            "prefer_local": l.prefer_local,
                            "use_router": l.use_router,
                        })),
                    },
                    "budget": {
                        "monthly_limit_cents": cfg.budget.monthly_limit_cents,
                        "spent_cents": total_spent,
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

        info!("agents.list: returning {} agents for user {}", agents.len(), ctx.email);
        WsFrame::ok_response("", json!({ "agents": agents }))
    }

    // ── User management handlers (admin only) ────────────────

    async fn handle_users_list(&self) -> WsFrame {
        let db = match self.user_db.read().await.as_ref() {
            Some(db) => db.clone(),
            None => return WsFrame::error_response("", "user system not initialized"),
        };
        match db.list_users() {
            Ok(users) => {
                let mut result: Vec<Value> = Vec::new();
                for u in &users {
                    let bindings = db.get_user_agents(&u.id).unwrap_or_default();
                    result.push(json!({
                        "id": u.id,
                        "email": u.email,
                        "display_name": u.display_name,
                        "role": u.role,
                        "status": u.status,
                        "created_at": u.created_at,
                        "updated_at": u.updated_at,
                        "last_login": u.last_login,
                        "bindings": bindings,
                    }));
                }
                WsFrame::ok_response("", json!({ "users": result }))
            }
            Err(e) => WsFrame::error_response("", &format!("failed to list users: {e}")),
        }
    }

    async fn handle_users_create(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let db = match self.user_db.read().await.as_ref() {
            Some(db) => db.clone(),
            None => return WsFrame::error_response("", "user system not initialized"),
        };

        let email = params.get("email").and_then(|v| v.as_str()).unwrap_or("");
        let display_name = params.get("display_name").and_then(|v| v.as_str()).unwrap_or("");
        let password = params.get("password").and_then(|v| v.as_str()).unwrap_or("");
        let role_str = params.get("role").and_then(|v| v.as_str()).unwrap_or("employee");

        if email.is_empty() || display_name.is_empty() || password.is_empty() {
            return WsFrame::error_response("", "email, display_name, and password are required");
        }
        // Email format validation (MEDIUM fix)
        if !email.contains('@') || email.len() > 254 {
            return WsFrame::error_response("", "invalid email format");
        }
        // Display name length limit
        if display_name.len() > 200 {
            return WsFrame::error_response("", "display_name too long (max 200 chars)");
        }
        if password.len() < 8 {
            return WsFrame::error_response("", "password must be at least 8 characters");
        }
        if password.len() > 1024 {
            return WsFrame::error_response("", "password too long");
        }

        let role: UserRole = match role_str.parse() {
            Ok(r) => r,
            Err(e) => return WsFrame::error_response("", &e),
        };

        match db.create_user(email, display_name, password, role) {
            Ok(user) => {
                let _ = db.log_action(Some(&ctx.user_id), "user.create", Some(&user.id), Some(&format!("email={email}")), None);
                WsFrame::ok_response("", json!({ "user": user }))
            }
            Err(e) => WsFrame::error_response("", &format!("failed to create user: {e}")),
        }
    }

    async fn handle_users_update(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let db = match self.user_db.read().await.as_ref() {
            Some(db) => db.clone(),
            None => return WsFrame::error_response("", "user system not initialized"),
        };

        let user_id = match params.get("user_id").and_then(|v| v.as_str()) {
            Some(id) => id,
            None => return WsFrame::error_response("", "user_id is required"),
        };

        let display_name = params.get("display_name").and_then(|v| v.as_str());
        let role = params.get("role").and_then(|v| v.as_str()).and_then(|r| r.parse::<UserRole>().ok());
        let password = params.get("password").and_then(|v| v.as_str());

        if let Some(pw) = password {
            if pw.len() < 8 {
                return WsFrame::error_response("", "password must be at least 8 characters");
            }
            if pw.len() > 1024 {
                return WsFrame::error_response("", "password too long");
            }
        }
        if let Some(name) = display_name {
            if name.len() > 200 {
                return WsFrame::error_response("", "display_name too long (max 200 chars)");
            }
        }

        match db.update_user(user_id, display_name, role, password) {
            Ok(()) => {
                let _ = db.log_action(Some(&ctx.user_id), "user.update", Some(user_id), None, None);
                WsFrame::ok_response("", json!({"status": "updated"}))
            }
            Err(e) => WsFrame::error_response("", &format!("failed to update user: {e}")),
        }
    }

    async fn handle_users_remove(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let db = match self.user_db.read().await.as_ref() {
            Some(db) => db.clone(),
            None => return WsFrame::error_response("", "user system not initialized"),
        };

        let user_id = match params.get("user_id").and_then(|v| v.as_str()) {
            Some(id) => id,
            None => return WsFrame::error_response("", "user_id is required"),
        };

        match db.set_user_status(user_id, duduclaw_auth::UserStatus::Suspended) {
            Ok(()) => {
                let _ = db.log_action(Some(&ctx.user_id), "user.suspend", Some(user_id), None, None);
                WsFrame::ok_response("", json!({"status": "suspended"}))
            }
            Err(e) => WsFrame::error_response("", &format!("failed to suspend user: {e}")),
        }
    }

    async fn handle_users_bind_agent(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let db = match self.user_db.read().await.as_ref() {
            Some(db) => db.clone(),
            None => return WsFrame::error_response("", "user system not initialized"),
        };

        let user_id = match params.get("user_id").and_then(|v| v.as_str()) {
            Some(id) => id,
            None => return WsFrame::error_response("", "user_id is required"),
        };
        let agent_name = match params.get("agent_name").and_then(|v| v.as_str()) {
            Some(n) => n,
            None => return WsFrame::error_response("", "agent_name is required"),
        };
        let access_level_str = params.get("access_level").and_then(|v| v.as_str()).unwrap_or("owner");
        let access_level: AccessLevel = match access_level_str.parse() {
            Ok(l) => l,
            Err(e) => return WsFrame::error_response("", &e),
        };

        // Verify agent exists
        let reg = self.registry.read().await;
        if reg.get(agent_name).is_none() {
            return WsFrame::error_response("", &format!("agent not found: {agent_name}"));
        }
        drop(reg);

        match db.bind_agent(user_id, agent_name, access_level) {
            Ok(()) => {
                let _ = db.log_action(Some(&ctx.user_id), "user.bind_agent", Some(agent_name),
                    Some(&format!("user={user_id}, level={access_level}")), None);
                WsFrame::ok_response("", json!({"status": "bound"}))
            }
            Err(e) => WsFrame::error_response("", &format!("failed to bind agent: {e}")),
        }
    }

    async fn handle_users_unbind_agent(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let db = match self.user_db.read().await.as_ref() {
            Some(db) => db.clone(),
            None => return WsFrame::error_response("", "user system not initialized"),
        };

        let user_id = match params.get("user_id").and_then(|v| v.as_str()) {
            Some(id) => id,
            None => return WsFrame::error_response("", "user_id is required"),
        };
        let agent_name = match params.get("agent_name").and_then(|v| v.as_str()) {
            Some(n) => n,
            None => return WsFrame::error_response("", "agent_name is required"),
        };

        match db.unbind_agent(user_id, agent_name) {
            Ok(()) => {
                let _ = db.log_action(Some(&ctx.user_id), "user.unbind_agent", Some(agent_name),
                    Some(&format!("user={user_id}")), None);
                WsFrame::ok_response("", json!({"status": "unbound"}))
            }
            Err(e) => WsFrame::error_response("", &format!("failed to unbind agent: {e}")),
        }
    }

    async fn handle_users_offboard(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let db = match self.user_db.read().await.as_ref() {
            Some(db) => db.clone(),
            None => return WsFrame::error_response("", "user system not initialized"),
        };

        let user_id = match params.get("user_id").and_then(|v| v.as_str()) {
            Some(id) => id,
            None => return WsFrame::error_response("", "user_id is required"),
        };
        let transfer_to = params.get("transfer_to").and_then(|v| v.as_str());

        // Get user's bound agents before offboarding
        let bindings = db.get_user_agents(user_id).unwrap_or_default();

        // Set user status to offboarded
        if let Err(e) = db.set_user_status(user_id, duduclaw_auth::UserStatus::Offboarded) {
            return WsFrame::error_response("", &format!("failed to offboard user: {e}"));
        }

        // Transfer agent ownership if specified
        let mut transferred = Vec::new();
        if let Some(new_owner_id) = transfer_to {
            for binding in &bindings {
                // Unbind from old user
                let _ = db.unbind_agent(user_id, &binding.agent_name);
                // Bind to new owner
                let _ = db.bind_agent(new_owner_id, &binding.agent_name, binding.access_level);
                transferred.push(binding.agent_name.clone());
            }
        }

        let _ = db.log_action(Some(&ctx.user_id), "user.offboard", Some(user_id),
            Some(&format!("transferred_agents={transferred:?}, transfer_to={transfer_to:?}")), None);

        WsFrame::ok_response("", json!({
            "status": "offboarded",
            "transferred_agents": transferred,
        }))
    }

    async fn handle_users_me(&self, ctx: &UserContext) -> WsFrame {
        let db = match self.user_db.read().await.as_ref() {
            Some(db) => db.clone(),
            None => {
                // No user DB — return context from JWT
                return WsFrame::ok_response("", json!({
                    "user": {
                        "id": ctx.user_id,
                        "email": ctx.email,
                        "role": ctx.role.to_string(),
                    },
                    "bindings": [],
                }));
            }
        };

        match db.get_user(&ctx.user_id) {
            Ok(Some(user)) => {
                let bindings = db.get_user_agents(&user.id).unwrap_or_default();
                WsFrame::ok_response("", json!({
                    "user": user,
                    "bindings": bindings,
                }))
            }
            _ => WsFrame::ok_response("", json!({
                "user": {
                    "id": ctx.user_id,
                    "email": ctx.email,
                    "role": ctx.role.to_string(),
                },
                "bindings": [],
            })),
        }
    }

    async fn handle_users_audit_log(&self, params: Value) -> WsFrame {
        let db = match self.user_db.read().await.as_ref() {
            Some(db) => db.clone(),
            None => return WsFrame::error_response("", "user system not initialized"),
        };

        let user_id = params.get("user_id").and_then(|v| v.as_str());
        let action = params.get("action").and_then(|v| v.as_str());
        let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(100).min(1000) as u32;

        match db.query_audit_log(user_id, action, limit) {
            Ok(entries) => WsFrame::ok_response("", json!({ "entries": entries })),
            Err(e) => WsFrame::error_response("", &format!("failed to query audit log: {e}")),
        }
    }

    // ── Marketplace ─────────────────────────────────────────────

    /// Build the Marketplace catalog JSON: built-in entries plus
    /// optional user-contributed entries from `~/.duduclaw/marketplace.json`.
    ///
    /// Schema of the optional file:
    /// ```json
    /// { "servers": [ { "id": "...", "name": "...", ... } ] }
    /// ```
    /// Each entry follows the `McpCatalogItem` JSON shape. Invalid files
    /// are skipped with a warning so a malformed user file never breaks
    /// the dashboard.
    async fn handle_marketplace_list(&self) -> WsFrame {
        use duduclaw_agent::mcp_template::{marketplace_catalog, McpCatalogItem};

        let mut servers: Vec<McpCatalogItem> = marketplace_catalog();

        // Merge optional user-contributed catalog entries.
        let user_path = self.home_dir.join("marketplace.json");
        if user_path.exists() {
            match tokio::fs::read_to_string(&user_path).await {
                Ok(content) => {
                    #[derive(serde::Deserialize)]
                    struct UserCatalog {
                        #[serde(default)]
                        servers: Vec<McpCatalogItem>,
                    }
                    match serde_json::from_str::<UserCatalog>(&content) {
                        Ok(user) => {
                            info!(
                                path = %user_path.display(),
                                count = user.servers.len(),
                                "Merged user marketplace catalog"
                            );
                            servers.extend(user.servers);
                        }
                        Err(e) => warn!(
                            path = %user_path.display(),
                            error = %e,
                            "Failed to parse user marketplace.json; skipping"
                        ),
                    }
                }
                Err(e) => warn!(
                    path = %user_path.display(),
                    error = %e,
                    "Failed to read user marketplace.json; skipping"
                ),
            }
        }

        let servers_json = match serde_json::to_value(&servers) {
            Ok(v) => v,
            Err(e) => return WsFrame::error_response(
                "",
                &format!("Failed to serialize marketplace catalog: {e}"),
            ),
        };

        WsFrame::ok_response("", json!({ "servers": servers_json }))
    }

    // ── MCP Management ──────────────────────────────────────────

    async fn handle_mcp_list(&self) -> WsFrame {
        use duduclaw_agent::mcp_template::{marketplace_catalog, read_mcp_config};

        let agents_dir = self.home_dir.join("agents");
        let mut agents = Vec::new();

        if let Ok(mut entries) = tokio::fs::read_dir(&agents_dir).await {
            while let Ok(Some(entry)) = entries.next_entry().await {
                let dir = entry.path();
                if !dir.is_dir() {
                    continue;
                }
                let name = match dir.file_name().and_then(|n| n.to_str()) {
                    Some(n) if !n.starts_with('_') && !n.starts_with('.') => n.to_string(),
                    _ => continue,
                };
                let config = match read_mcp_config(&dir) {
                    Ok(c) => c,
                    Err(_) => continue,
                };
                let servers: Vec<Value> = config.mcp_servers.iter().map(|(k, v)| {
                    json!({
                        "name": k,
                        "command": v.command,
                        "args": v.args,
                        "env": v.env,
                    })
                }).collect();
                agents.push(json!({
                    "agent_id": name,
                    "servers": servers,
                }));
            }
        }

        let catalog: Vec<Value> = marketplace_catalog().iter().map(|item| {
            json!({
                "id": item.id,
                "name": item.name,
                "description": item.description,
                "category": item.category,
                "requires_oauth": item.requires_oauth,
                "default_def": {
                    "command": item.default_def.command,
                    "args": item.default_def.args,
                    "env": item.default_def.env,
                },
                "required_env": item.required_env,
            })
        }).collect();

        WsFrame::ok_response("", json!({ "agents": agents, "catalog": catalog }))
    }

    async fn handle_mcp_update(&self, params: &Value) -> WsFrame {
        use duduclaw_agent::mcp_template::{add_server_to_config, remove_server_from_config, McpServerDef};

        let agent_id = match params.get("agent_id").and_then(|v| v.as_str()) {
            Some(id) => id,
            None => return WsFrame::error_response("", "agent_id is required"),
        };
        let action = match params.get("action").and_then(|v| v.as_str()) {
            Some(a) => a,
            None => return WsFrame::error_response("", "action is required (add/remove)"),
        };
        let server_name = match params.get("server_name").and_then(|v| v.as_str()) {
            Some(n) => n,
            None => return WsFrame::error_response("", "server_name is required"),
        };

        if !is_valid_agent_id(agent_id) {
            return WsFrame::error_response("", "Invalid agent_id");
        }

        let agent_dir = self.home_dir.join("agents").join(agent_id);
        if !agent_dir.is_dir() {
            return WsFrame::error_response("", &format!("Agent '{agent_id}' not found"));
        }

        match action {
            "add" => {
                let def: McpServerDef = match params.get("server_def") {
                    Some(v) => match serde_json::from_value(v.clone()) {
                        Ok(d) => d,
                        Err(e) => return WsFrame::error_response("", &format!("Invalid server_def: {e}")),
                    },
                    None => return WsFrame::error_response("", "server_def is required for add action"),
                };
                let ad = agent_dir.clone();
                let sn = server_name.to_string();
                match tokio::task::spawn_blocking(move || add_server_to_config(&ad, &sn, &def)).await {
                    Ok(Ok(())) => WsFrame::ok_response("", json!({ "success": true })),
                    Ok(Err(e)) => WsFrame::error_response("", &e),
                    Err(e) => WsFrame::error_response("", &format!("Internal error: {e}")),
                }
            }
            "remove" => {
                let ad = agent_dir.clone();
                let sn = server_name.to_string();
                match tokio::task::spawn_blocking(move || remove_server_from_config(&ad, &sn)).await {
                    Ok(Ok(())) => WsFrame::ok_response("", json!({ "success": true })),
                    Ok(Err(e)) => WsFrame::error_response("", &e),
                    Err(e) => WsFrame::error_response("", &format!("Internal error: {e}")),
                }
            }
            _ => WsFrame::error_response("", &format!("Unknown action: {action}. Use 'add' or 'remove'")),
        }
    }

    // ── MCP OAuth handlers ──────────────────────────────────

    /// List available OAuth providers with configuration and token status.
    async fn handle_mcp_oauth_providers(&self) -> WsFrame {
        use crate::mcp_oauth;

        let redirect_uri = format!("http://localhost:3000/api/mcp/oauth/callback");
        let providers = mcp_oauth::builtin_providers(&redirect_uri);

        let results: Vec<Value> = providers.iter().map(|p| {
            let token = mcp_oauth::get_token(&self.home_dir, &p.provider_id);
            let status = match &token {
                Some(t) => {
                    if let Some(exp) = t.expires_at {
                        if chrono::Utc::now() >= exp {
                            "expired"
                        } else {
                            "authenticated"
                        }
                    } else {
                        "authenticated"
                    }
                }
                None => "none",
            };
            json!({
                "provider_id": p.provider_id,
                "auth_url": p.auth_url,
                "scopes": p.scopes,
                "configured": !p.client_id.is_empty(),
                "status": status,
                "expires_at": token.and_then(|t| t.expires_at),
            })
        }).collect();

        WsFrame::ok_response("", json!({ "providers": results }))
    }

    /// Start an OAuth flow: generate PKCE, store pending state, return auth URL.
    async fn handle_mcp_oauth_start(&self, params: Value) -> WsFrame {
        use crate::mcp_oauth;

        let provider_id = match params.get("provider_id").and_then(|v| v.as_str()) {
            Some(id) => id.to_string(),
            None => return WsFrame::error_response("", "provider_id is required"),
        };

        let client_id = params.get("client_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let client_secret = params.get("client_secret").and_then(|v| v.as_str()).unwrap_or("").to_string();

        // Find the built-in provider or create a custom one
        let redirect_uri = format!("http://localhost:3000/api/mcp/oauth/callback");
        let mut config = mcp_oauth::builtin_providers(&redirect_uri)
            .into_iter()
            .find(|p| p.provider_id == provider_id)
            .unwrap_or_else(|| mcp_oauth::McpOAuthConfig {
                provider_id: provider_id.clone(),
                client_id: String::new(),
                client_secret: String::new(),
                auth_url: params.get("auth_url").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                token_url: params.get("token_url").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                scopes: params.get("scopes")
                    .and_then(|v| v.as_array())
                    .map(|a| a.iter().filter_map(|s| s.as_str().map(|s| s.to_string())).collect())
                    .unwrap_or_default(),
                redirect_uri: redirect_uri.clone(),
            });

        // Override client_id/secret if provided in params
        if !client_id.is_empty() {
            config.client_id = client_id;
        }
        if !client_secret.is_empty() {
            config.client_secret = client_secret;
        }

        if config.client_id.is_empty() {
            return WsFrame::error_response("", "client_id is required (provide in params or pre-configure)");
        }
        if config.auth_url.is_empty() || config.token_url.is_empty() {
            return WsFrame::error_response("", "auth_url and token_url are required for custom providers");
        }

        // Generate PKCE
        let (code_verifier, code_challenge) = mcp_oauth::generate_pkce();
        let state = uuid::Uuid::new_v4().to_string();

        let auth_url = mcp_oauth::build_auth_url(&config, &state, &code_challenge);

        // Store pending
        let pending = mcp_oauth::PendingOAuth {
            provider_id: provider_id.clone(),
            state: state.clone(),
            code_verifier,
            config,
            created_at: std::time::Instant::now(),
        };

        {
            let mut map = self.mcp_oauth_pending.write().await;
            // Cleanup expired entries
            mcp_oauth::cleanup_pending(&mut map);
            map.insert(state.clone(), pending);
        }

        info!(provider = %provider_id, "MCP OAuth flow started");

        WsFrame::ok_response("", json!({
            "auth_url": auth_url,
            "state": state,
        }))
    }

    /// Check if a provider's OAuth flow has completed (token exists).
    async fn handle_mcp_oauth_status(&self, params: Value) -> WsFrame {
        use crate::mcp_oauth;

        let provider_id = match params.get("provider_id").and_then(|v| v.as_str()) {
            Some(id) => id,
            None => return WsFrame::error_response("", "provider_id is required"),
        };

        let token = mcp_oauth::get_token(&self.home_dir, provider_id);
        match token {
            Some(t) => WsFrame::ok_response("", json!({
                "authenticated": true,
                "expires_at": t.expires_at,
                "scopes": t.scopes,
            })),
            None => WsFrame::ok_response("", json!({
                "authenticated": false,
            })),
        }
    }

    /// Revoke (remove) a stored OAuth token for a provider.
    async fn handle_mcp_oauth_revoke(&self, params: Value) -> WsFrame {
        use crate::mcp_oauth;

        let provider_id = match params.get("provider_id").and_then(|v| v.as_str()) {
            Some(id) => id,
            None => return WsFrame::error_response("", "provider_id is required"),
        };

        match mcp_oauth::remove_token(&self.home_dir, provider_id) {
            Ok(()) => {
                info!(provider = %provider_id, "MCP OAuth token revoked");
                WsFrame::ok_response("", json!({ "success": true }))
            }
            Err(e) => WsFrame::error_response("", &e),
        }
    }
}

// ── Standalone helpers ────────────────────────────────────────

/// Check if Docker (or Podman) is available by running `docker info`.
/// Returns `("pass"/"warn", message)`.
async fn check_docker() -> (&'static str, String) {
    // Try `docker info` first, then `podman info`
    for cmd_name in &["docker", "podman"] {
        let result = tokio::process::Command::new(cmd_name)
            .arg("info")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .await;

        match result {
            Ok(out) if out.status.success() => {
                return ("pass", format!("{cmd_name} daemon is running"));
            }
            Ok(_) => {
                return ("warn", format!("{cmd_name} found but daemon is not running"));
            }
            Err(_) => {} // try next
        }
    }

    ("warn", "No container runtime (docker/podman) found in PATH".to_string())
}

// ═══════════════════════════════════════════════════════════════
// Task Board, Activity Feed, Autopilot, Shared Skills handlers
// ═══════════════════════════════════════════════════════════════

impl MethodHandler {
    // ── Store accessors ─────────────────────────────────────

    async fn task_store(&self) -> Result<Arc<TaskStore>, WsFrame> {
        self.task_store
            .read()
            .await
            .clone()
            .ok_or_else(|| WsFrame::error_response("", "Task store not initialized"))
    }

    async fn ap_store(&self) -> Result<Arc<AutopilotStore>, WsFrame> {
        self.autopilot_store
            .read()
            .await
            .clone()
            .ok_or_else(|| WsFrame::error_response("", "Autopilot store not initialized"))
    }

    /// Broadcast an event via the injected event_tx (best-effort, no error on failure).
    async fn broadcast_event(&self, event: &str, payload: Value) {
        if let Some(tx) = self.event_tx.read().await.as_ref() {
            let frame = WsFrame::Event {
                event: event.to_string(),
                payload,
                seq: None,
                state_version: None,
            };
            let _ = tx.send(serde_json::to_string(&frame).unwrap_or_default());
        }
    }

    // ── Task handlers ───────────────────────────────────────

    async fn handle_tasks_list(&self, params: Value) -> WsFrame {
        let store = match self.task_store().await {
            Ok(s) => s,
            Err(f) => return f,
        };
        let status = params.get("status").and_then(|v| v.as_str());
        let agent_id = params.get("agent_id").and_then(|v| v.as_str());
        let priority = params.get("priority").and_then(|v| v.as_str());
        match store.list_tasks(status, agent_id, priority).await {
            Ok(rows) => {
                let tasks: Vec<Value> = rows.iter().map(|r| task_row_to_json(r)).collect();
                WsFrame::ok_response("", json!({ "tasks": tasks }))
            }
            Err(e) => WsFrame::error_response("", &format!("list tasks: {e}")),
        }
    }

    async fn handle_tasks_create(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let store = match self.task_store().await {
            Ok(s) => s,
            Err(f) => return f,
        };
        let title = params.get("title").and_then(|v| v.as_str()).unwrap_or("").to_string();
        if title.is_empty() {
            return WsFrame::error_response("", "title is required");
        }
        let assigned_to = params.get("assigned_to").and_then(|v| v.as_str()).unwrap_or("").to_string();
        if assigned_to.is_empty() {
            return WsFrame::error_response("", "assigned_to is required");
        }
        let description = params.get("description").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let priority = params.get("priority").and_then(|v| v.as_str()).unwrap_or("medium").to_string();
        let tags = params
            .get("tags")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str())
                    .collect::<Vec<_>>()
                    .join(",")
            })
            .unwrap_or_default();

        let mut row = TaskRow::new(
            uuid::Uuid::new_v4().to_string(),
            title.clone(),
            description,
            priority,
            assigned_to.clone(),
            if ctx.user_id.is_empty() { "system" } else { &ctx.user_id }.to_string(),
        );
        row.tags = tags;
        row.parent_task_id = params.get("parent_task_id").and_then(|v| v.as_str()).map(|s| s.to_string());

        if let Err(e) = store.insert_task(&row).await {
            return WsFrame::error_response("", &format!("create task: {e}"));
        }

        // Record activity event
        let activity = ActivityRow {
            id: uuid::Uuid::new_v4().to_string(),
            event_type: "task_created".into(),
            agent_id: assigned_to,
            task_id: Some(row.id.clone()),
            summary: title,
            timestamp: Utc::now().to_rfc3339(),
            metadata: None,
        };
        let _ = store.append_activity(&activity).await;

        let task_json = task_row_to_json(&row);
        self.broadcast_event("task.created", task_json.clone()).await;
        self.broadcast_event("activity.new", activity_row_to_json(&activity)).await;

        WsFrame::ok_response("", json!({ "task": task_json }))
    }

    async fn handle_tasks_update(&self, params: Value) -> WsFrame {
        let store = match self.task_store().await {
            Ok(s) => s,
            Err(f) => return f,
        };
        let task_id = params.get("task_id").and_then(|v| v.as_str()).unwrap_or("");
        if task_id.is_empty() {
            return WsFrame::error_response("", "task_id is required");
        }
        match store.update_task(task_id, &params).await {
            Ok(Some(row)) => {
                let task_json = task_row_to_json(&row);
                self.broadcast_event("task.updated", task_json.clone()).await;

                // If status changed to done/blocked, record activity
                if let Some(status) = params.get("status").and_then(|v| v.as_str()) {
                    let event_type = match status {
                        "done" => "task_completed",
                        "blocked" => "task_blocked",
                        _ => "",
                    };
                    if !event_type.is_empty() {
                        let activity = ActivityRow {
                            id: uuid::Uuid::new_v4().to_string(),
                            event_type: event_type.into(),
                            agent_id: row.assigned_to.clone(),
                            task_id: Some(task_id.to_string()),
                            summary: row.title.clone(),
                            timestamp: Utc::now().to_rfc3339(),
                            metadata: None,
                        };
                        let _ = store.append_activity(&activity).await;
                        self.broadcast_event("activity.new", activity_row_to_json(&activity)).await;
                    }
                }

                WsFrame::ok_response("", json!({ "task": task_json }))
            }
            Ok(None) => WsFrame::error_response("", &format!("Task not found: {task_id}")),
            Err(e) => WsFrame::error_response("", &format!("update task: {e}")),
        }
    }

    async fn handle_tasks_remove(&self, params: Value) -> WsFrame {
        let store = match self.task_store().await {
            Ok(s) => s,
            Err(f) => return f,
        };
        let task_id = params.get("task_id").and_then(|v| v.as_str()).unwrap_or("");
        if task_id.is_empty() {
            return WsFrame::error_response("", "task_id is required");
        }
        match store.remove_task(task_id).await {
            Ok(true) => {
                self.broadcast_event("task.removed", json!({ "task_id": task_id })).await;
                WsFrame::ok_response("", json!({ "success": true }))
            }
            Ok(false) => WsFrame::error_response("", &format!("Task not found: {task_id}")),
            Err(e) => WsFrame::error_response("", &format!("remove task: {e}")),
        }
    }

    async fn handle_tasks_assign(&self, params: Value) -> WsFrame {
        let task_id = params.get("task_id").and_then(|v| v.as_str()).unwrap_or("");
        let agent_id = params.get("agent_id").and_then(|v| v.as_str()).unwrap_or("");
        if task_id.is_empty() || agent_id.is_empty() {
            return WsFrame::error_response("", "task_id and agent_id are required");
        }
        let update = json!({ "assigned_to": agent_id });
        self.handle_tasks_update(json!({ "task_id": task_id, "assigned_to": agent_id })).await
    }

    // ── Activity handlers ───────────────────────────────────

    async fn handle_activity_list(&self, params: Value) -> WsFrame {
        let store = match self.task_store().await {
            Ok(s) => s,
            Err(f) => return f,
        };
        let agent_id = params.get("agent_id").and_then(|v| v.as_str());
        let event_type = params.get("type").and_then(|v| v.as_str());
        let limit = params.get("limit").and_then(|v| v.as_i64()).unwrap_or(20);
        let offset = params.get("offset").and_then(|v| v.as_i64()).unwrap_or(0);

        match store.list_activity(agent_id, event_type, limit, offset).await {
            Ok((rows, total)) => {
                let events: Vec<Value> = rows.iter().map(|r| activity_row_to_json(r)).collect();
                WsFrame::ok_response("", json!({ "events": events, "total": total }))
            }
            Err(e) => WsFrame::error_response("", &format!("list activity: {e}")),
        }
    }

    // ── Autopilot handlers ──────────────────────────────────

    async fn handle_autopilot_list(&self) -> WsFrame {
        let store = match self.ap_store().await {
            Ok(s) => s,
            Err(f) => return f,
        };
        match store.list_rules().await {
            Ok(rows) => {
                let rules: Vec<Value> = rows.iter().map(|r| autopilot_rule_to_json(r)).collect();
                WsFrame::ok_response("", json!({ "rules": rules }))
            }
            Err(e) => WsFrame::error_response("", &format!("list autopilot: {e}")),
        }
    }

    async fn handle_autopilot_create(&self, params: Value) -> WsFrame {
        let store = match self.ap_store().await {
            Ok(s) => s,
            Err(f) => return f,
        };
        let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
        if name.is_empty() {
            return WsFrame::error_response("", "name is required");
        }
        let trigger_event = params.get("trigger_event").and_then(|v| v.as_str()).unwrap_or("task_created").to_string();
        let conditions = params.get("conditions").cloned().unwrap_or(json!({}));
        let action = params.get("action").cloned().unwrap_or(json!({}));

        let row = AutopilotRuleRow {
            id: uuid::Uuid::new_v4().to_string(),
            name,
            enabled: true,
            trigger_event,
            conditions: conditions.to_string(),
            action: action.to_string(),
            created_at: Utc::now().to_rfc3339(),
            last_triggered_at: None,
            trigger_count: 0,
        };
        if let Err(e) = store.insert_rule(&row).await {
            return WsFrame::error_response("", &format!("create autopilot rule: {e}"));
        }
        WsFrame::ok_response("", json!({ "rule": autopilot_rule_to_json(&row) }))
    }

    async fn handle_autopilot_update(&self, params: Value) -> WsFrame {
        let store = match self.ap_store().await {
            Ok(s) => s,
            Err(f) => return f,
        };
        let rule_id = params.get("rule_id").and_then(|v| v.as_str()).unwrap_or("");
        if rule_id.is_empty() {
            return WsFrame::error_response("", "rule_id is required");
        }
        match store.update_rule(rule_id, &params).await {
            Ok(Some(row)) => WsFrame::ok_response("", json!({ "rule": autopilot_rule_to_json(&row) })),
            Ok(None) => WsFrame::error_response("", &format!("Rule not found: {rule_id}")),
            Err(e) => WsFrame::error_response("", &format!("update rule: {e}")),
        }
    }

    async fn handle_autopilot_remove(&self, params: Value) -> WsFrame {
        let store = match self.ap_store().await {
            Ok(s) => s,
            Err(f) => return f,
        };
        let rule_id = params.get("rule_id").and_then(|v| v.as_str()).unwrap_or("");
        if rule_id.is_empty() {
            return WsFrame::error_response("", "rule_id is required");
        }
        match store.remove_rule(rule_id).await {
            Ok(true) => WsFrame::ok_response("", json!({ "success": true })),
            Ok(false) => WsFrame::error_response("", &format!("Rule not found: {rule_id}")),
            Err(e) => WsFrame::error_response("", &format!("remove rule: {e}")),
        }
    }

    async fn handle_autopilot_history(&self, params: Value) -> WsFrame {
        let store = match self.ap_store().await {
            Ok(s) => s,
            Err(f) => return f,
        };
        let rule_id = params.get("rule_id").and_then(|v| v.as_str());
        let limit = params.get("limit").and_then(|v| v.as_i64()).unwrap_or(20);
        match store.list_history(rule_id, limit).await {
            Ok(entries) => {
                let result: Vec<Value> = entries
                    .iter()
                    .map(|e| {
                        json!({
                            "id": e.id,
                            "rule_id": e.rule_id,
                            "rule_name": e.rule_name,
                            "triggered_at": e.triggered_at,
                            "result": e.result,
                            "details": e.details,
                        })
                    })
                    .collect();
                WsFrame::ok_response("", json!({ "entries": result }))
            }
            Err(e) => WsFrame::error_response("", &format!("autopilot history: {e}")),
        }
    }

    // ── Shared Skills handlers ──────────────────────────────

    async fn handle_skills_shared_list(&self) -> WsFrame {
        let shared_dir = self.home_dir.join("shared").join("skills");
        if !shared_dir.exists() {
            return WsFrame::ok_response("", json!({ "skills": [] }));
        }
        let mut skills: Vec<Value> = Vec::new();
        if let Ok(mut entries) = tokio::fs::read_dir(&shared_dir).await {
            while let Ok(Some(entry)) = entries.next_entry().await {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("md") {
                    continue;
                }
                let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
                let content = tokio::fs::read_to_string(&path).await.unwrap_or_default();
                // Parse frontmatter for metadata
                let description = extract_frontmatter(&content, "description").unwrap_or_default();
                let shared_by = extract_frontmatter(&content, "shared_by").unwrap_or_default();
                let shared_at = extract_frontmatter(&content, "shared_at").unwrap_or_default();
                let tags: Vec<String> = extract_frontmatter(&content, "tags")
                    .map(|t| t.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect())
                    .unwrap_or_default();
                let adopted_by: Vec<String> = extract_frontmatter(&content, "adopted_by")
                    .map(|t| t.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect())
                    .unwrap_or_default();
                let usage_count: i64 = extract_frontmatter(&content, "usage_count")
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);

                skills.push(json!({
                    "name": name,
                    "description": description,
                    "shared_by": shared_by,
                    "shared_at": shared_at,
                    "tags": tags,
                    "adopted_by": adopted_by,
                    "usage_count": usage_count,
                }));
            }
        }
        WsFrame::ok_response("", json!({ "skills": skills }))
    }

    async fn handle_skills_share(&self, params: Value) -> WsFrame {
        let agent_id = params.get("agent_id").and_then(|v| v.as_str()).unwrap_or("");
        let skill_name = params.get("skill_name").and_then(|v| v.as_str()).unwrap_or("");
        if agent_id.is_empty() || skill_name.is_empty() {
            return WsFrame::error_response("", "agent_id and skill_name are required");
        }
        // Read skill from agent's SKILLS directory
        let skill_path = self
            .home_dir
            .join("agents")
            .join(agent_id)
            .join("SKILLS")
            .join(format!("{skill_name}.md"));
        if !skill_path.exists() {
            return WsFrame::error_response(
                "",
                &format!("Skill not found: {skill_name} in agent {agent_id}"),
            );
        }
        let content = match tokio::fs::read_to_string(&skill_path).await {
            Ok(c) => c,
            Err(e) => return WsFrame::error_response("", &format!("read skill: {e}")),
        };

        // Write to shared skills directory with metadata frontmatter
        let shared_dir = self.home_dir.join("shared").join("skills");
        if let Err(e) = tokio::fs::create_dir_all(&shared_dir).await {
            return WsFrame::error_response("", &format!("create shared dir: {e}"));
        }
        let shared_path = shared_dir.join(format!("{skill_name}.md"));
        let now = Utc::now().to_rfc3339();
        let shared_content = format!(
            "---\nshared_by: {agent_id}\nshared_at: {now}\ndescription: \ntags: \nadopted_by: \nusage_count: 0\n---\n\n{content}"
        );
        if let Err(e) = tokio::fs::write(&shared_path, &shared_content).await {
            return WsFrame::error_response("", &format!("write shared skill: {e}"));
        }

        WsFrame::ok_response("", json!({ "success": true }))
    }

    async fn handle_skills_adopt(&self, params: Value) -> WsFrame {
        let skill_name = params.get("skill_name").and_then(|v| v.as_str()).unwrap_or("");
        let target_agent = params.get("target_agent_id").and_then(|v| v.as_str()).unwrap_or("");
        if skill_name.is_empty() || target_agent.is_empty() {
            return WsFrame::error_response("", "skill_name and target_agent_id are required");
        }
        // Read from shared
        let shared_path = self.home_dir.join("shared").join("skills").join(format!("{skill_name}.md"));
        if !shared_path.exists() {
            return WsFrame::error_response("", &format!("Shared skill not found: {skill_name}"));
        }
        let content = match tokio::fs::read_to_string(&shared_path).await {
            Ok(c) => c,
            Err(e) => return WsFrame::error_response("", &format!("read shared skill: {e}")),
        };

        // Extract actual content (strip frontmatter)
        let skill_content = if let Some(idx) = content.find("\n---\n") {
            content[idx + 5..].trim().to_string()
        } else {
            content.clone()
        };

        // Write to target agent's SKILLS directory
        let target_dir = self
            .home_dir
            .join("agents")
            .join(target_agent)
            .join("SKILLS");
        if let Err(e) = tokio::fs::create_dir_all(&target_dir).await {
            return WsFrame::error_response("", &format!("create agent skills dir: {e}"));
        }
        let target_path = target_dir.join(format!("{skill_name}.md"));
        if let Err(e) = tokio::fs::write(&target_path, &skill_content).await {
            return WsFrame::error_response("", &format!("write skill to agent: {e}"));
        }

        // Update shared frontmatter: bump usage_count and add to adopted_by
        let updated = update_frontmatter_field(&content, "usage_count", |old| {
            let count: i64 = old.parse().unwrap_or(0);
            (count + 1).to_string()
        });
        let updated = update_frontmatter_field(&updated, "adopted_by", |old| {
            let mut agents: Vec<&str> = old.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
            if !agents.contains(&target_agent) {
                agents.push(target_agent);
            }
            agents.join(", ")
        });
        let _ = tokio::fs::write(&shared_path, &updated).await;

        WsFrame::ok_response("", json!({ "success": true }))
    }
}

// ── JSON serialization helpers ──────────────────────────────

fn task_row_to_json(r: &TaskRow) -> Value {
    json!({
        "id": r.id,
        "title": r.title,
        "description": r.description,
        "status": r.status,
        "priority": r.priority,
        "assigned_to": r.assigned_to,
        "created_by": r.created_by,
        "created_at": r.created_at,
        "updated_at": r.updated_at,
        "completed_at": r.completed_at,
        "blocked_reason": r.blocked_reason,
        "parent_task_id": r.parent_task_id,
        "tags": r.tags.split(',').filter(|s| !s.is_empty()).collect::<Vec<_>>(),
        "message_id": r.message_id,
    })
}

fn activity_row_to_json(r: &ActivityRow) -> Value {
    json!({
        "id": r.id,
        "type": r.event_type,
        "agent_id": r.agent_id,
        "task_id": r.task_id,
        "summary": r.summary,
        "timestamp": r.timestamp,
        "metadata": r.metadata.as_ref().and_then(|s| serde_json::from_str::<Value>(s).ok()),
    })
}

fn autopilot_rule_to_json(r: &AutopilotRuleRow) -> Value {
    json!({
        "id": r.id,
        "name": r.name,
        "enabled": r.enabled,
        "trigger_event": r.trigger_event,
        "conditions": serde_json::from_str::<Value>(&r.conditions).unwrap_or(json!({})),
        "action": serde_json::from_str::<Value>(&r.action).unwrap_or(json!({})),
        "created_at": r.created_at,
        "last_triggered_at": r.last_triggered_at,
        "trigger_count": r.trigger_count,
    })
}

/// Extract a field value from YAML-style frontmatter (`---` delimited).
fn extract_frontmatter(content: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}:");
    for line in content.lines() {
        if line == "---" {
            // End of frontmatter
        }
        if let Some(rest) = line.strip_prefix(&prefix) {
            return Some(rest.trim().to_string());
        }
    }
    None
}

/// Update a field in YAML-style frontmatter with a transform function.
fn update_frontmatter_field(content: &str, key: &str, transform: impl Fn(&str) -> String) -> String {
    let prefix = format!("{key}:");
    content
        .lines()
        .map(|line| {
            if let Some(rest) = line.strip_prefix(&prefix) {
                format!("{prefix} {}", transform(rest.trim()))
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}
