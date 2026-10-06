use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use chrono::{DateTime, Datelike, Duration as ChronoDuration, Utc};
use duduclaw_agent::registry::AgentRegistry;
use duduclaw_auth::acl;
use duduclaw_auth::models::{AccessLevel, UserRole};
use duduclaw_auth::{self, JwtConfig, UserContext, UserDb};
use duduclaw_core::traits::MemoryEngine;
use duduclaw_core::truncate_bytes;
use duduclaw_memory::{SqliteMemoryEngine, is_system_signal};
use rusqlite::params;
use serde_json::{Value, json};
use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::autopilot_store::{AutopilotRuleRow, AutopilotStore};
use crate::cron_scheduler::CronScheduler;
use crate::cron_store::{CronStore, CronTaskRow};
use crate::distributor_store::{
    DistributorInput, DistributorPatch, DistributorStore, IssuedLicense,
};
use crate::extension::GatewayExtension;
use crate::gvu::stagnation::{GvuStagnationConfig, stagnation_snapshot};
use crate::gvu::telemetry::telemetry_summary;
use crate::gvu::version_store::VersionStore;
use crate::partner_store::{
    PartnerCustomerInput, PartnerCustomerPatch, PartnerProfileInput, PartnerStore,
};
use crate::playbook;
use crate::protocol::WsFrame;
use crate::secaudit_reports;
use crate::task_store::{
    ActivityRow, CommentRow, PlanRow, PlanStepRow, TaskIterationRow, TaskRow, TaskStore,
};

mod util_agent;
mod agent_toml_util;
mod mcp_keys_util;
mod capabilities_apply;
mod os_watch_apply;
mod gates;
mod doctor_util;
mod autopilot_util;
mod runtime_apply;
mod wiki_scope_util;
mod odoo_config;
mod inference_wire;
mod identity_config;
mod inference_apply;
mod contract_config;
mod redaction_config;
mod redaction_sources;
mod redaction_wire;
mod memory_util;
mod skill_synthesis_config;
mod killswitch_config;
mod config_util;
mod lifecycle;
mod handshake_license;
mod tools_catalog;
mod agents_basic;
mod agents_create;
mod templates_util;
mod templates_rpc;
mod templates_create;
mod agents_ops;
mod agents_update;
mod contract_rpc;
mod redaction_rpc;
mod identity_rpc;
mod inference_rpc;
mod mcp_keys_rpc;
mod killswitch_rpc;
mod wiki_scope_rpc;
mod agents_archive;
mod agents_avatar;
mod agents_inspect;
mod channels_status;
mod channels_add;
mod channels_test;
mod channels_bind;
mod channels_config;
mod channels_hot;
mod accounts_basic;
mod memory_rpc;
mod memory_history_rpc;
mod wiki_rpc;
mod shared_wiki_rpc;
mod search_rpc;
mod skills_rpc;
mod skills_install_request;
mod skills_custom;
mod cron_rpc;
mod partner_rpc;
mod branding_rpc;
mod distributor_rpc;
mod cli_login_rpc;
mod setup_token_rpc;
mod system_status;
mod experts_rpc;
mod system_config_rpc;
mod audit_rpc;
mod security_rpc;
mod analytics_rpc;
mod evolution_rpc;
mod playbook_rpc;
mod channel_util;
mod runtime_models_rpc;
mod system_update_config;
mod delegation_rpc;
mod task_forward_rpc;
mod accounts_add_rpc;
mod config_io;
mod cost_rpc;
mod odoo_util;
mod odoo_configure;
mod odoo_schema;
mod odoo_agent_rpc;
mod agents_list_rpc;
mod dashboard_layout;
mod widgets_custom;
mod departments_rpc;
mod users_rpc;
mod marketplace_rpc;
mod mcp_manifest;
mod mcp_import;
mod mcp_rpc;
mod mcp_oauth;
mod google_rpc;
mod accounts_util;
mod tasks_rpc;
mod discovery_rpc;
mod tasks_detail_rpc;
mod task_privacy;
mod push_filter;
mod artifact_evidence;
mod workflow_errors;
#[cfg(test)]
mod task_privacy_tests;
#[cfg(test)]
mod f5_privacy_tests;
#[cfg(test)]
mod workflow_review_tests;
mod responsibilities_rpc;
mod workflow_review_rpc;
mod workflow_drafts_rpc;
mod workflow_runs_rpc;
mod plans_rpc;
mod activity_timeline_rpc;
mod runs_rpc;
mod chat_sessions_rpc;
mod fork_migrate_rpc;
mod approvals_topology_rpc;
mod mail_rpc;
mod approvals_decide;
mod approval_decider;
mod growth_rpc;
mod autopilot_rpc;
mod ticks_rpc;
mod os_rpc;
mod redaction_status_rpc;
mod skills_share_rpc;
mod task_json;
mod timeline_util;
mod chat_history_util;
mod runs_util;
mod canvas_rpc;
mod device_rpc;
mod device_timedate_rpc;
mod maintenance_rpc;
mod device_backup;
mod dispatch_core;
mod dispatch_knowledge;
mod dispatch_system;
mod dispatch_org;
mod dispatch_ops;
pub(crate) mod config_commit;
mod config_schema;
mod system_update_config_v168;
mod tick_sources_rpc;
mod channel_ingress_rpc;
mod computer_workspaces_rpc;
mod config_raw_rpc;
mod agents_update_v168;
#[cfg(test)]
mod tests;

pub(crate) use util_agent::*;
pub(crate) use agent_toml_util::*;
pub(crate) use mcp_keys_util::*;
pub(crate) use capabilities_apply::*;
pub(crate) use os_watch_apply::*;
pub use gates::*;
pub use doctor_util::*;
pub(crate) use autopilot_util::*;
pub(crate) use runtime_apply::*;
pub(crate) use wiki_scope_util::*;
pub(crate) use odoo_config::*;
pub(crate) use inference_wire::*;
pub(crate) use identity_config::*;
pub(crate) use inference_apply::*;
pub(crate) use contract_config::*;
pub use redaction_config::*;
pub(crate) use redaction_sources::*;
pub(crate) use redaction_wire::*;
pub(crate) use memory_util::*;
pub(crate) use skill_synthesis_config::*;
pub(crate) use killswitch_config::*;
pub(crate) use config_util::*;
pub(crate) use channel_util::*;
pub(crate) use accounts_util::*;
pub(crate) use task_json::*;
pub(crate) use task_privacy::*;
pub use push_filter::{PUSH_CACHE_SECS, PushGate, filter_push_event};
pub(crate) use timeline_util::*;
pub(crate) use chat_history_util::*;
pub(crate) use runs_util::*;
pub use device_backup::*;

/// Dispatches incoming RPC methods to the appropriate handler.
pub struct MethodHandler {
    workflow_store: tokio::sync::OnceCell<Arc<crate::workflow::WorkflowStore>>,
    workflow_service: tokio::sync::OnceCell<Arc<crate::workflow::WorkflowService>>,
    registry: Arc<RwLock<AgentRegistry>>,
    home_dir: PathBuf,
    start_time: Instant,
    channel_status: Arc<RwLock<std::collections::HashMap<String, ChannelState>>>,
    heartbeat: RwLock<Option<Arc<duduclaw_agent::HeartbeatScheduler>>>,
    /// Reply context for hot-starting channels after config changes.
    reply_ctx: RwLock<Option<Arc<crate::channel_reply::ReplyContext>>>,
    /// Handles for running channel bot tasks (for hot-stop on remove).
    channel_handles:
        tokio::sync::Mutex<std::collections::HashMap<String, tokio::task::JoinHandle<()>>>,
    /// [M2] Server-side cached pending update (set by check_update, consumed by apply_update).
    pending_update: RwLock<Option<PendingUpdate>>,
    /// User database for multi-user auth (injected after gateway start).
    user_db: RwLock<Option<Arc<UserDb>>>,
    /// JWT configuration for token issuance (injected after gateway start).
    jwt_config: RwLock<Option<Arc<JwtConfig>>>,
    /// Plugin extension point (NullExtension by default).
    extension: Arc<dyn GatewayExtension>,
    /// Explicit product form-factor override, injected after gateway start by
    /// the Cloud control-plane. `None` → resolve per-request from
    /// `DUDUCLAW_EDITION` env > license tier > `Personal`.
    edition_override: RwLock<Option<duduclaw_core::EditionProfile>>,
    /// Active interactive CLI-login sessions ("Dashboard 一鍵登入"), keyed by
    /// session id. Each drives a CLI's native login command in a PTY.
    cli_auth_sessions: RwLock<std::collections::HashMap<String, Arc<crate::cli_auth::AuthSession>>>,
    /// WP-D: single global "訂閱帳號" setup-token wizard slot. Unlike
    /// `cli_auth_sessions` (many concurrent sessions, one per runtime login),
    /// this wizard is deliberately single-flight — a second `start` cancels
    /// whatever is currently in progress. See `setup_token_wizard.rs`.
    setup_token_session: RwLock<Option<crate::setup_token_wizard::SetupTokenSlot>>,
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
    /// Typed event broadcast sender consumed by `AutopilotEngine`.
    autopilot_event_tx:
        RwLock<Option<tokio::sync::broadcast::Sender<crate::autopilot_engine::AutopilotEvent>>>,
    /// RFC-23 redaction manager. `None` ⇒ pipeline disabled at this layer.
    redaction_manager: RwLock<Option<Arc<duduclaw_redaction::RedactionManager>>>,
    /// RFC-23 poison state — set when redaction was *requested* but could not
    /// be resolved (unparseable `[redaction]`, manager that refuses to open).
    /// Distinct from `redaction_manager: None`, which is the legitimate
    /// "not configured" state. See DESIGN-redaction-field-rules-2026-09 §12.
    redaction_poison: RwLock<Option<RedactionPoison>>,
    /// Vault GC task paired with the live redaction manager — restarted on
    /// every hot swap so exactly one sweeper runs against the active vault.
    redaction_gc: tokio::sync::Mutex<Option<duduclaw_redaction::GcTask>>,
    /// M1/M60: long-lived SQLite-backed audit/reliability index, lazily opened
    /// once and synced by a background task — so audit/reliability RPCs and the
    /// `/api/reliability/summary` HTTP endpoint reuse one connection instead of
    /// opening a fresh DB + running a full `sync_from_files` on every request.
    audit_index: tokio::sync::OnceCell<Arc<crate::evolution_events::query::AuditEventIndex>>,
    /// SQLite message queue shared with the goal-loop driver (injected after
    /// gateway start). Needed to rebuild the driver on a hot config reload.
    message_queue: RwLock<Option<Arc<crate::message_queue::MessageQueue>>>,
    /// Handles for the long-lived background drivers whose config is read once
    /// at startup (`goal_loop`, `topology_evolution`). Keyed by a stable
    /// `&'static str`. Hot config reload aborts the old handle and re-spawns
    /// with fresh config (same abort+respawn pattern as `channel_handles`).
    driver_handles:
        tokio::sync::Mutex<std::collections::HashMap<&'static str, tokio::task::JoinHandle<()>>>,
    /// Shared registry of per-agent OS filesystem watchers. Lets `agents.update`
    /// hot stop/start one agent's `[os_watch]` watcher without a gateway restart.
    os_watchers: Arc<crate::os_events::OsWatcherRegistry>,
    /// Shared registry of per-agent frontmost (foreground app/window) poll
    /// tasks (P4-3). Symmetric with `os_watchers` — `agents.update` /
    /// `os.settings.update` hot stop/start one agent's polling after a
    /// `frontmost_poll_secs` / `os_native` edit.
    os_frontmost: Arc<crate::os_frontmost::OsFrontmostRegistry>,
    /// Handler-held digital-footprint aggregation tracker (P4-3). Membership is
    /// interior-mutable so `os.settings.update` can hot enable/disable an
    /// agent's `[os_watch] footprint` without a gateway restart. Background
    /// ingest + distill tasks are spawned once in `server.rs`.
    footprint: Arc<crate::footprint_distill::FootprintTracker>,
    /// WP-A9: A3 task-forward-model, shared between the `DispatchEngine`
    /// settle hook (constructed in `server.rs`) and the `GoalLoopDriver`
    /// predict hook (built in `respawn_goal_loop_driver` below) so both
    /// hooks read/write the SAME in-memory statistical-bucket cache instead
    /// of two independently-loaded, never-reconciled copies. `None` unless
    /// `[task_forward_model] enabled = true` at gateway startup (design
    /// §7.3 — this field is never constructed at all when disabled, not
    /// merely inert).
    forward_model: RwLock<Option<Arc<crate::prediction::task_forward_store::TaskForwardModel>>>,
    /// Resident sensing (WP4): shared tick-observation state — the SAME
    /// `Arc<TickHub>` the `tick_source` poll tasks and `AutopilotEngine`
    /// write into (`server.rs` injects it via `set_tick_hub` once, right
    /// after constructing it). `None` until injected, and permanently
    /// `None` when the autopilot engine block never ran (no task/autopilot
    /// store) — `ticks.sources`/`ticks.recent` treat that identically to
    /// "feature never started": zero counts, empty records, never an error.
    tick_hub: RwLock<Option<Arc<crate::tick_source::TickHub>>>,
    /// v1.68: what is needed to respawn the `[[tick.sources]]` poll tasks
    /// after a dashboard edit (bus sender, events store, hub, live handles).
    /// `None` when the autopilot engine block never ran — then a `[tick]`
    /// edit reports `restart_required` instead.
    tick_runtime: tokio::sync::Mutex<Option<tick_sources_rpc::TickRuntime>>,
}

/// Cached update info from the last `system.check_update` call. [M2][R2:NM1]
#[derive(Clone)]
struct PendingUpdate {
    download_url: String,
    checksum_url: String,
    version: String,
    /// The full check result, kept ONLY when an extension-supplied
    /// [`crate::updater::UpdateProvider`] produced it: that path resolves its
    /// asset at apply time (short-TTL signed URLs), so there is no URL pair to
    /// cache and `apply` needs the whole descriptor back. `None` on the CE
    /// GitHub path, which stays exactly as it was.
    info: Option<crate::updater::UpdateInfo>,
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
    /// Route `method` to the correct handler and return a [`WsFrame`] response.
    ///
    /// `request_id` is carried through so that all response frames are correctly
    /// correlated with the originating client request.
    ///
    /// In-process entry point: carries no transport facts, so it dispatches
    /// with [`RpcConnInfo::internal`] — `peer: None` (which reads as **not**
    /// loopback, fail-closed) and `pre_auth: false`. Callers that DO own a
    /// socket (`server.rs::handle_socket`) use [`Self::handle_conn`].
    pub async fn handle(&self, method: &str, params: Value, ctx: &UserContext) -> WsFrame {
        self.handle_conn(
            method,
            params,
            ctx,
            crate::power_local::RpcConnInfo::internal(),
        )
        .await
    }

    /// Route `method` with the originating connection's transport facts
    /// attached (peer address + whether the WS handshake carried any
    /// credential). Only `server.rs::handle_socket` has that information;
    /// everything else goes through [`Self::handle`].
    pub async fn handle_conn(
        &self,
        method: &str,
        params: Value,
        ctx: &UserContext,
        conn: crate::power_local::RpcConnInfo,
    ) -> WsFrame {
        self.dispatch(method, params, ctx, conn).await
    }

    /// Internal dispatch — returns a WsFrame with placeholder id (overwritten by caller).
    async fn dispatch(
        &self,
        method: &str,
        params: Value,
        ctx: &UserContext,
        conn: crate::power_local::RpcConnInfo,
    ) -> WsFrame {
        // ── Pre-auth connection gate (appliance lock screen) ─────────────
        // Runs before EVERY other gate, including the forced-password-change
        // and edition gates below: those two answer "may this identity do
        // that?", and a pre-auth connection has no identity at all. A WS
        // connection that completed the handshake with no credential
        // (`server.rs::handle_socket`, which only grants that on an appliance
        // over loopback) may reach exactly ONE method — see
        // `power_local::PRE_AUTH_ALLOWED_METHOD` and that module's header for
        // why a login-free power control is the right call on a lock screen.
        if conn.pre_auth && !crate::power_local::is_pre_auth_allowlisted(method) {
            return login_required_reject_frame();
        }

        // ── Forced password-change gate ─────────────────────────────────
        // Runs FIRST — before the edition gate, before the plugin extension
        // dispatch, before the method match. Which features exist has no
        // bearing on whether this account may use them yet: a caller flagged
        // `must_change_password` gets exactly the self-service allowlist in
        // `is_password_change_allowlisted`, full stop. See that function's
        // doc comment for why the WS handshake itself no longer refuses these
        // callers (TODO-bootstrap-admin-ws-deadlock.md).
        if ctx.requires_password_change() && !is_password_change_allowlisted(method) {
            return must_change_password_reject_frame();
        }

        // ── Edition gate (G2) ────────────────────────────────
        // The single server-side chokepoint for the Enterprise-only RPC
        // surface. It runs BEFORE the extension dispatch and before the method
        // match, so no plugin, legacy route alias or hand-written WebSocket
        // client can reach a multi-person management surface from a Personal
        // install. `is_enterprise_only_method` carries the list + rationale;
        // the (cheap, allocation-free) string test comes first so the ordinary
        // RPC path never pays for edition resolution.
        if is_enterprise_only_method(method) && self.resolve_edition_profile().await.is_personal() {
            return enterprise_only_reject_frame();
        }

        // ── Plugin extension dispatch ──────
        // Try extension first; if it returns Some, the method is handled.
        if let Some(frame) = self
            .extension
            .handle_method(method, params.clone(), ctx)
            .await
        {
            return frame;
        }
        self.dispatch_core(method, params, ctx, conn).await
    }
}
