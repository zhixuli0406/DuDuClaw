use axum::body::Bytes;
use axum::{
    Json, Router,
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    extract::{ConnectInfo, DefaultBodyLimit, Multipart},
    extract::{Query, State},
    response::IntoResponse,
    routing::{get, post},
};
use futures_util::{SinkExt, StreamExt};
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::sync::broadcast;
use tracing::{error, info, warn};

use duduclaw_auth::{JwtConfig, UserContext, UserDb};
use duduclaw_memory::causal::{CausalStore, CausalStoreError, EvidenceScope};
use duduclaw_memory::causal_identify::AdjustmentReadiness;

static WS_RATE_LIMITER: std::sync::LazyLock<Mutex<HashMap<IpAddr, (Instant, u32)>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

fn check_ws_rate_limit(ip: IpAddr) -> bool {
    let mut map = WS_RATE_LIMITER.lock().unwrap_or_else(|e| e.into_inner());
    let now = Instant::now();
    // Cleanup stale entries every time the map grows large
    if map.len() > 1000 {
        map.retain(|_, (t, _)| now.duration_since(*t).as_secs() < 120);
    }
    let entry = map.entry(ip).or_insert((now, 0));
    if now.duration_since(entry.0).as_secs() > 60 {
        *entry = (now, 1);
        return true;
    }
    entry.1 += 1;
    entry.1 <= 30 // max 30 WS connections per minute per IP
}

/// Login attempt rate limiter: max 5 attempts per (IP, email) per 15 minutes.
///
/// M2: previously keyed by email alone and never reset on success, which let a
/// remote attacker lock out any known account for 15 minutes simply by sending
/// bad passwords. Now the key includes the source IP (so one attacker IP cannot
/// exhaust the limit for a victim on a different IP) and a successful login
/// clears the counter (`reset_login_rate_limit`).
static LOGIN_RATE_LIMITER: std::sync::LazyLock<Mutex<HashMap<(IpAddr, String), (Instant, u32)>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

/// Returns `true` if the attempt from `(ip, email)` is within the rate budget.
fn check_login_rate_limit(ip: IpAddr, email: &str) -> bool {
    let mut map = LOGIN_RATE_LIMITER.lock().unwrap_or_else(|e| e.into_inner());
    let now = Instant::now();
    if map.len() > 10000 {
        map.retain(|_, (t, _)| now.duration_since(*t).as_secs() < 900);
    }
    let entry = map.entry((ip, email.to_string())).or_insert((now, 0));
    if now.duration_since(entry.0).as_secs() > 900 {
        *entry = (now, 1);
        return true;
    }
    entry.1 += 1;
    entry.1 <= 5
}

/// Clear the failed-attempt counter for `(ip, email)` after a successful login
/// so a legitimate user is never penalised for earlier typos (M2).
fn reset_login_rate_limit(ip: IpAddr, email: &str) {
    let mut map = LOGIN_RATE_LIMITER.lock().unwrap_or_else(|e| e.into_inner());
    map.remove(&(ip, email.to_string()));
}

/// Per-IP rate limit for OTP *verification* (Haiku review #2/#3). The engine
/// already caps 5 attempts per challenge and 3 live challenges per account, but
/// verify itself had no IP throttle — a distributed guesser could try many
/// codes across challenges. This bounds verify attempts to 10 per IP per minute.
static OTP_VERIFY_RATE_LIMITER: std::sync::LazyLock<Mutex<HashMap<IpAddr, (Instant, u32)>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

fn check_otp_verify_rate_limit(ip: IpAddr) -> bool {
    let mut map = OTP_VERIFY_RATE_LIMITER
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let now = Instant::now();
    if map.len() > 10000 {
        map.retain(|_, (t, _)| now.duration_since(*t).as_secs() < 60);
    }
    let entry = map.entry(ip).or_insert((now, 0));
    if now.duration_since(entry.0).as_secs() > 60 {
        *entry = (now, 1);
        return true;
    }
    entry.1 += 1;
    entry.1 <= 10
}

use crate::auth::AuthManager;
use crate::decision_brief::BriefEvidenceSelection;
use crate::decision_calibration::KnownDayInputs;
use crate::decision_empirical::{EmpiricalSamplingMode, EmpiricalSensitivityPlan};
use crate::decision_model_review::OutcomeModelReviewCriteria;
use crate::decision_operator_import::{MAX_OPERATOR_IMPORT_BODY_BYTES, OperatorPilotImportRequest};
use crate::decision_policy::{JointRiskScreenCriteria, StaffingResourcePlan};
use crate::decision_sensitivity::BoundedCount;
use crate::decision_shadow_screen::ShadowReviewCriteria;
use crate::decision_sla_shadow_dashboard::MAX_SHADOW_SLA_SOURCE_BODY_BYTES;
use crate::decision_store::{
    CausalSourceRemoval, DecisionScope, DecisionStore, DecisionStoreError,
};
use crate::extension::GatewayExtension;
use crate::handlers::MethodHandler;
use crate::protocol::WsFrame;
use crate::synthetic_connector_adapter::{
    SyntheticAdapterError, SyntheticLifecycleKind, SyntheticLocalConnectorAdapter,
};

/// Configuration for the WebSocket RPC gateway.
pub struct GatewayConfig {
    /// Bind address (e.g. `"0.0.0.0"`).
    pub bind: String,
    /// Port to listen on.
    pub port: u16,
    /// Optional authentication token.  When `None`, authentication is
    /// disabled.
    pub auth_token: Option<String>,
    /// Path to the DuDuClaw home directory (e.g. `~/.duduclaw`).
    pub home_dir: std::path::PathBuf,
    /// Extra allowed dashboard `Origin`s for WebSocket/CORS, beyond the built-in
    /// loopback hosts. Sourced from config.toml `[gateway] allowed_origins` +
    /// `DUDUCLAW_ALLOWED_ORIGINS`. Empty (default) => loopback-only, zero change.
    /// Entries may be `host`, `host:port`, or a full origin (scheme stripped on
    /// load). Needed when the dashboard is reached over a tailnet/proxy hostname.
    pub allowed_origins: Vec<String>,
    /// Plugin extension point. Defaults to [`NullExtension`].
    pub extension: Arc<dyn GatewayExtension>,
    /// Explicit product form-factor override. `None` means resolve at request
    /// time from `DUDUCLAW_EDITION` env > license tier > `Personal`. Cloud
    /// control-plane sets `Some(..)` (or the env var) per managed tenant.
    pub edition: Option<duduclaw_core::EditionProfile>,
}

/// Internal shared state for the Axum application.
struct AppState {
    auth: AuthManager,
    handler: MethodHandler,
    tx: broadcast::Sender<String>,
    /// Broadcast channel for real-time events (channel status, etc.) pushed to clients.
    event_tx: broadcast::Sender<String>,
    /// User database for multi-user authentication.
    user_db: Arc<UserDb>,
    /// JWT configuration for token issuance and verification.
    jwt_config: Arc<JwtConfig>,
    /// Channel-DM delivery for passwordless login OTP codes (WP12). Injected so
    /// the pre-auth OTP handler never needs raw channel config / secret manager.
    otp_delivery: Arc<dyn crate::otp_delivery::OtpDeliverer>,
    /// DuDuClaw home directory (`~/.duduclaw`). Used by the voice endpoints to
    /// read `[voice]` STT/TTS config from `config.toml`.
    home_dir: std::path::PathBuf,
}

/// Start the WebSocket RPC gateway and block until it shuts down.
pub async fn start_gateway(config: GatewayConfig) -> duduclaw_core::error::Result<()> {
    // Initialise the log broadcast channel (must happen before subscribers connect).
    let log_tx = crate::log::init_log_broadcaster();
    let tx = log_tx;
    // Boot reference for the /healthz scheduler-staleness probe.
    SERVER_START_UNIX.store(
        chrono::Utc::now().timestamp(),
        std::sync::atomic::Ordering::Relaxed,
    );

    let home_dir = config.home_dir.clone();

    // ── B5 (OS security line P0): boot event ──────────────────────────────
    // One `os_boot` row per gateway process start, chained (B1) into
    // `security_audit.jsonl` — the durable "when did this box last come up,
    // running what" record neither `journald` drop-ins nor the OS timers
    // (later waves) can substitute for from inside this process. Cheap,
    // synchronous, best-effort (same fire-and-forget convention as every
    // other `append_audit_event` call site in this codebase — an audit-write
    // failure must never block boot). `running_image_version()` reads
    // `/etc/os-release`'s `IMAGE_VERSION=` and is `None` off-appliance.
    duduclaw_security::audit::log_os_boot(
        &home_dir,
        crate::os_update::running_image_version().as_deref(),
    );

    // ── WP-G1: apply a pending device-migration restore, if any ──────────
    // Must run before anything else in this function touches `home_dir`'s
    // files (config.toml, identity.key, org.toml, MCP key, …) — those are
    // exactly the files a restore replaces, so doing this first means this
    // boot's own bootstrap steps below always see the POST-restore state,
    // never a stale mix. Cheap no-op (`Ok(None)`) on every boot with no
    // pending marker — the overwhelming majority — so this costs nothing on
    // a non-appliance install or an appliance box that never ran
    // `device.backup_restore`. See `backup_restore.rs`'s module doc for the
    // full ordering guarantee (old data is always preserved, never deleted).
    match crate::backup_restore::perform_pending_restore_swap(
        &home_dir,
        &chrono::Utc::now().format("%Y%m%dT%H%M%S%.3fZ").to_string(),
    ) {
        Ok(Some(report)) => {
            crate::metrics::global_metrics().backup_restore_swap_ok();
            info!(
                preserved = %report.preserved_dir.display(),
                entries = report.entries_swapped,
                "device migration restore applied at boot — previous data preserved"
            );
        }
        Ok(None) => {}
        Err(e) => {
            crate::metrics::global_metrics().backup_restore_swap_fail();
            tracing::error!(
                error = ?e,
                "device migration restore failed at boot — gateway continues with whatever \
                 state is on disk; if old data was already preserved it is under a \
                 restore-backup-<timestamp> directory in the home dir"
            );
        }
    }

    // ── System-settings app: re-apply a persisted static wired-network
    // config, if any (see `network::wired::reapply_wired_config_on_boot`'s
    // own doc for WHY — the sysd verb's effect lives on tmpfs, so it does
    // not survive a reboot on its own). Spawned rather than awaited so a
    // slow/unresponsive `duduclaw-sysd` can never delay the rest of boot;
    // the function itself no-ops instantly off-appliance or with nothing
    // persisted, and every failure is logged, never propagated.
    {
        let home_dir = home_dir.clone();
        tokio::spawn(async move {
            crate::network::wired::reapply_wired_config_on_boot(&home_dir).await;
        });
    }

    // ── H3g-b: surface a failed /data migration to the dashboard ─────────
    // `duduclaw-data-migrate.service` runs before this process and, on
    // failure, records `<home>/system/migrations.failed.json` — nothing
    // ever read that back until now. Spawned (not awaited) for the same
    // reason as the wired-config reapply just above: a slow/failing
    // task-store open must never delay the rest of boot. See
    // `migration_alert.rs`'s module doc for the one-time-per-failure dedup
    // contract.
    {
        let home_dir = home_dir.clone();
        tokio::spawn(async move {
            crate::migration_alert::check_and_notify(&home_dir).await;
        });
    }

    // ── Memory-db split self-heal (2026-08-20 關鍵洞察 incident) ─────────
    // Merge any per-agent `agents/<id>/[state/]memory.db` back into the
    // shared `<home>/memory.db` and archive the source file, restoring the
    // invariant `handlers.rs::agent_memory_db_path` relies on (reads prefer
    // the per-agent file when it exists, but the live write path is the
    // shared file). Runs before any subsystem opens memory engines. Cheap
    // no-op when no per-agent files exist — the overwhelming majority of
    // boots. Failures leave the source files in place and never abort boot.
    {
        let report = crate::memory_migrate::merge_per_agent_memory_dbs(&home_dir);
        if report.merged_files > 0 {
            info!(
                files = report.merged_files,
                memories = report.memories_rows,
                key_facts = report.key_facts_rows,
                "per-agent memory.db files merged into shared memory.db"
            );
        }
        for e in &report.errors {
            tracing::warn!(error = %e, "per-agent memory.db merge failure (file left in place)");
        }
    }

    let extension = config.extension.clone();
    let edition_override = config.edition;
    {
        // Startup-time best-effort resolution for the boot log (license tier
        // may not be loaded yet; the live value is resolved per-request in
        // `MethodHandler::resolve_edition_profile`).
        let boot_edition = duduclaw_core::EditionProfile::resolve(
            std::env::var("DUDUCLAW_EDITION").ok().as_deref(),
            edition_override.map(|e| e.as_str()),
            None,
        );
        info!("edition_profile={}", boot_edition.as_str());
    }

    // Provision the internal MCP API key as early as possible (before any
    // child spawn) and record it via `set_internal_mcp_api_key`, from where
    // `mcp_forward_env_vars()` folds it into every MCP env assembly point
    // (per-runtime MCP config writers, `.mcp.json` template, tool-loop
    // client). Without this, the M6 fail-closed `mcp-server` auth (v1.31)
    // kills the tool surface of every runtime whose CLI spawns MCP children
    // with a sanitized env (the Grok "查 odoo 不行" incident). An
    // operator-provided env key always wins; provisioning failure is
    // warn-not-fatal (status quo: no key).
    if std::env::var(duduclaw_core::ENV_MCP_API_KEY)
        .map(|v| v.trim().is_empty())
        .unwrap_or(true)
    {
        match crate::mcp_internal_key::ensure_internal_mcp_key(&home_dir) {
            Ok(key) => {
                duduclaw_core::set_internal_mcp_api_key(key);
                info!(
                    "internal MCP API key active for this gateway (spawned MCP children authenticate)"
                );
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "internal MCP key provisioning failed — CLI-spawned duduclaw \
                     mcp-server children will fail auth unless DUDUCLAW_MCP_API_KEY \
                     is provided in the environment"
                );
            }
        }
    }

    // WP21 debt ⑧ — mint `<home>/identity.key` if absent, before any MCP env
    // block is assembled, so every `.mcp.json` / runtime config written later
    // in this boot carries a signable `DUDUCLAW_AGENT_TOKEN`. Never rotates an
    // existing key (that would invalidate tokens held by live CLI children).
    // Failure is warn-not-fatal: no key ⇒ `IdentityVerdict::Disabled` ⇒ the
    // pre-WP21 behaviour, which is exactly the right degradation.
    match duduclaw_core::ensure_identity_key(&home_dir) {
        Ok(_) => {
            let strict = duduclaw_core::require_identity_token_from_home(&home_dir);
            info!(
                require_identity_token = strict,
                "MCP caller-identity signing key ready ({})",
                duduclaw_core::identity_key_path(&home_dir).display()
            );
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                "could not create the MCP caller-identity key — caller ids stay \
                 unverified (env-var impersonation remains possible); this does \
                 not affect any other functionality"
            );
        }
    }

    // WP22 T1 — bootstrap `<home>/org.toml`, the authoritative record of who
    // reports to whom. Seeding happens **once**, only when the file is absent:
    // re-importing every boot would re-open the very hole this file closes
    // (tamper with `agent.toml`, wait for a restart, watch the tampered value
    // get promoted to authority). An operator who edits `agent.toml` by hand
    // adopts the change explicitly with `duduclaw org sync`; `duduclaw doctor`
    // reports the drift until they do. Agents with no record keep resolving
    // from their `agent.toml`, so a failure here degrades to the pre-WP22
    // behaviour rather than to an outage.
    match duduclaw_core::org_store::seed_if_absent(&home_dir) {
        Ok(Some(count)) => info!(
            agents = count,
            "organisational authority seeded at {} (one-time bootstrap from agent.toml)",
            duduclaw_core::org_store::org_store_path(&home_dir).display()
        ),
        Ok(None) => {
            let drift = duduclaw_core::org_store::detect_drift(&home_dir);
            if drift.is_empty() {
                info!("organisational authority loaded from org.toml");
            } else {
                tracing::warn!(
                    agents = drift.len(),
                    "org.toml disagrees with {} agent.toml mirror(s) — delegation uses \
                     org.toml; run `duduclaw org sync` (or fix via the dashboard org chart) \
                     to adopt the file edits. `duduclaw doctor` lists them.",
                    drift.len()
                );
            }
        }
        Err(e) => tracing::warn!(
            error = %e,
            "could not create org.toml — organisational authority falls back to each \
             agent.toml (pre-WP22 behaviour); delegation still works"
        ),
    }

    // Install operator-configured extra allowed Origins for dashboard WS/CORS.
    // Empty by default => built-in loopback origins only (no behaviour change).
    let extra_origins = init_allowed_origins(config.allowed_origins.clone());
    if extra_origins.is_empty() {
        info!("dashboard WS/CORS: loopback origins only (localhost / 127.0.0.1 / [::1])");
    } else {
        info!(
            "dashboard WS/CORS: {} extra allowed origin(s): {}",
            extra_origins.len(),
            extra_origins.join(", ")
        );
    }

    // ── BUG-2 fix: anchor EvolutionEvents audit log to home_dir, not cwd ──
    //
    // EvolutionEventLogger::from_env() falls back to cwd-relative
    // "data/evolution/events" if neither EVOLUTION_EVENTS_DIR nor DUDUCLAW_HOME
    // is set. When the gateway runs with cwd=$HOME, audit events are silently
    // dropped because the path doesn't exist. We pin both env vars before any
    // emitter is constructed so every component sees the same target.
    {
        let events_dir = home_dir.join("evolution").join("events");
        // SAFETY: process is single-threaded at this point in start_gateway
        // (no other tasks have been spawned yet). Setting env vars here is
        // safe; later threads only read.
        if std::env::var_os("EVOLUTION_EVENTS_DIR").is_none() {
            unsafe {
                std::env::set_var("EVOLUTION_EVENTS_DIR", &events_dir);
            }
            info!("EVOLUTION_EVENTS_DIR defaulted to {}", events_dir.display());
        }
        if std::env::var_os("DUDUCLAW_HOME").is_none() {
            unsafe {
                std::env::set_var("DUDUCLAW_HOME", &home_dir);
            }
        }
        // Run a synchronous-ish self-test so a misconfigured path surfaces at
        // boot rather than after the first prediction error.
        let logger = crate::evolution_events::logger::EvolutionEventLogger::from_env();
        if let Err(e) = logger.self_test().await {
            warn!(
                "EvolutionEvents audit log path {} is not writable: {e} — \
                 audit events will be silently dropped until this is fixed",
                events_dir.display()
            );
        }
    }

    let handler = MethodHandler::with_extension(config.home_dir, extension.clone()).await;
    handler.set_edition_override(edition_override).await;

    // Initialize cost telemetry (must happen before any Claude CLI calls)
    if let Err(e) = crate::cost_telemetry::init_telemetry(&home_dir) {
        tracing::warn!(error = %e, "Failed to initialize cost telemetry — continuing without it");
    }

    // ── RFC-23: redaction pipeline bootstrap ────────────────────
    // Reads `[redaction]` from config.toml. When `enabled = false`
    // (default) the manager is never built; existing behaviour is
    // unchanged. When enabled, `swap_redaction_manager` installs the
    // manager AND its paired vault-GC task (the handler owns both, so
    // `redaction.update` can later hot-swap them without a restart).
    //
    // Three outcomes, never conflated (DESIGN-redaction-field-rules-2026-09
    // §12, decision B): not configured ⇒ nothing; configured+enabled ⇒ build;
    // unparseable config OR a manager that refuses to open ⇒ **poison state**.
    // Poison is loud (ERROR log + Activity Feed + dashboard banner) and
    // recoverable via `redaction.update`; the gateway still boots, because
    // redaction of tool results happens in the `duduclaw mcp-server`
    // subprocess (which already refuses to start on a broken config), not here.
    {
        let cfg_path = home_dir.join("config.toml");
        let raw = match std::fs::read_to_string(&cfg_path) {
            Ok(s) => Some(s),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => {
                // Unreadable (permissions, I/O) is not "absent" — treat it as
                // a poison cause rather than silently running unprotected.
                let reason = crate::redaction_integration::poison_reason(format!(
                    "config.toml 無法讀取：{e}"
                ));
                error!(error = %e, "RFC-23 redaction: config.toml unreadable — entering poison state");
                handler
                    .set_redaction_poison(Some(crate::handlers::RedactionPoison::new(
                        reason.clone(),
                    )))
                    .await;
                crate::redaction_integration::post_redaction_activity(
                    &home_dir,
                    "redaction_init_failed",
                    &format!("去識別化保護未能啟動：{reason}"),
                )
                .await;
                None
            }
        };

        match crate::redaction_integration::classify_redaction_boot(raw.as_deref()) {
            crate::redaction_integration::BootOutcome::Enabled(rcfg) => {
                match crate::redaction_integration::build_manager_from_home(&home_dir, *rcfg) {
                    Ok(manager) => {
                        info!(
                            rules = manager.engine().rule_count(),
                            ttl_h = manager.vault_ttl_hours(),
                            "RFC-23 redaction pipeline enabled"
                        );
                        handler.swap_redaction_manager(Some(manager)).await;
                    }
                    Err(e) => {
                        let reason = crate::redaction_integration::poison_reason(e.to_string());
                        error!(
                            error = %e,
                            "RFC-23 redaction pipeline FAILED to initialise — \
                             gateway enters the redaction POISON state. Check \
                             config.toml [redaction] and ~/.duduclaw/redaction/."
                        );
                        handler
                            .set_redaction_poison(Some(crate::handlers::RedactionPoison::new(
                                reason.clone(),
                            )))
                            .await;
                        crate::redaction_integration::post_redaction_activity(
                            &home_dir,
                            "redaction_init_failed",
                            &format!("去識別化保護未能啟動：{reason}"),
                        )
                        .await;
                    }
                }
            }
            crate::redaction_integration::BootOutcome::Poisoned(reason) => {
                error!(
                    reason = %reason,
                    "RFC-23 redaction config could not be parsed — gateway enters \
                     the redaction POISON state (no redaction manager installed)."
                );
                handler
                    .set_redaction_poison(Some(crate::handlers::RedactionPoison::new(
                        reason.clone(),
                    )))
                    .await;
                crate::redaction_integration::post_redaction_activity(
                    &home_dir,
                    "redaction_init_failed",
                    &format!("去識別化保護未能啟動：{reason}"),
                )
                .await;
            }
            crate::redaction_integration::BootOutcome::Disabled => {
                tracing::debug!("Redaction pipeline not enabled in config.toml");
            }
        }
    }

    // ── First-run license seeding (E2, enterprise Docker distribution) ──
    //
    // Symmetric to the branding-bundle seeding above: when this binary ships
    // co-located with a signed OEM `license.json` (its path in the
    // `DUDUCLAW_LICENSE_FILE` env var — the compose pack mounts it read-only at
    // `/opt/license.json`), verify it against the baked issuer registry and copy
    // it into `~/.duduclaw/license.json` *before* the license runtime loads it,
    // so a customer `docker compose up` gets the baked license with zero
    // `duduclaw license activate`. Idempotent (never overwrites an existing
    // license) and fail-closed (an unverifiable candidate is skipped). The call
    // logs its own outcome; the return value is only for tests.
    let _ = crate::license_seed::seed_license_if_absent(&home_dir);

    // ── License runtime bootstrap ───────────────────────────────
    //
    // Loads ~/.duduclaw/license.json (when present), verifies its Ed25519
    // signature against trusted issuer public keys collected from
    // `DUDUCLAW_LICENSE_PUBKEY_<ID>` env vars, and spawns two background
    // tasks: a phone-home loop (refreshes the license on the cadence
    // dictated by features.toml) and a CRL poll (downgrades on emergency
    // revocations).
    //
    // Failure modes never crash the gateway: a missing license, an
    // empty key registry, signature mismatch, expired license, or
    // grace-period exceeded all collapse to OpenSource mode.
    let _license_runtime = {
        // Baked production issuer key (v2; v1 retired) + any operator env
        // overrides, so a stock binary verifies a DuDuClaw-issued license.json
        // with no extra setup — the enterprise upgrade path is "drop in
        // license.json → restart". Env-only + OpenSource until the v2 pubkey is
        // baked (see license_runtime::PROD_ISSUER_PUBKEY_HEX).
        let registry = crate::license_runtime::production_registry();
        let runtime =
            crate::license_runtime::LicenseRuntime::bootstrap(home_dir.clone(), registry).await;
        // Publish the runtime to the process-global slot so dashboard
        // RPCs and other gateway services can read the current tier
        // without having to thread a handle through the entire
        // initialisation chain.
        crate::license_runtime::set_global(runtime.clone());
        // Spawn the background phone-home + CRL polling tasks. The
        // returned JoinHandles are deliberately dropped — the tasks are
        // long-lived and use cooperative cancellation via the runtime
        // state itself, not handle abortion.
        let _tasks = runtime.spawn_background_tasks();
        runtime
    };

    // ── First-run branding bundle seeding (§11.2) ───────────────
    //
    // When this binary ships co-located with a signed branding.bundle.json
    // (DUDUCLAW_BRANDING_BUNDLE env / executable sibling / macOS .app
    // Resources), verify it against the baked issuer registry and copy it into
    // ~/.duduclaw/ *before* any branding::load reads it. Idempotent (never
    // overwrites an existing bundle) and fail-closed (an unverifiable candidate
    // is warned once and skipped). Runs after the license runtime so the same
    // production registry the branding verifier uses is warm; the desktop
    // sidecar path (`duduclaw run --yes` → start_gateway) is covered here too.
    // The call logs its own outcome; the return value is only for tests.
    let _ = crate::branding::seed_bundle_if_absent(&home_dir);

    // Initialize wiki trust store (Phase 2 of wiki RL trust feedback).
    // Best-effort: if open fails, the rest of the system still works — RAG
    // simply falls back to frontmatter trust and trust feedback is skipped.
    {
        let trust_db = home_dir.join("wiki_trust.db");
        let pre_existing = trust_db.exists();

        // Phase 7: read [wiki.trust_feedback] + [wiki.trust_feedback.janitor]
        // from config.toml. Missing/malformed → safe defaults.
        let (trust_cfg, janitor_cfg, federation_cfg) = {
            let raw = std::fs::read_to_string(home_dir.join("config.toml")).unwrap_or_default();
            let table: toml::Table = raw.parse().unwrap_or_default();
            (
                duduclaw_memory::trust_store::TrustStoreConfig::from_toml(&table),
                duduclaw_memory::JanitorConfig::from_toml(&table),
                crate::wiki_trust_federation::FederationConfig::from_toml(&table),
            )
        };

        // R4 DEBT-3: propagate the configured tracker cap to the
        // process-global feedback module before any traffic arrives.
        duduclaw_memory::feedback::set_max_active_conversations(trust_cfg.max_active_conversations);
        match duduclaw_memory::trust_store::init_global_trust_store_with_config(
            &trust_db, trust_cfg,
        ) {
            Ok(store) => {
                info!(
                    path = %trust_db.display(),
                    cap = trust_cfg.per_conversation_cap,
                    archive_threshold = trust_cfg.archive_threshold,
                    daily_limit = trust_cfg.daily_signal_limit,
                    "Wiki trust store initialized"
                );

                // Phase 7 migration: on first creation of the trust DB, seed
                // rows from existing wiki frontmatter so `trust_audit` shows
                // a meaningful baseline immediately. Idempotent for re-runs.
                if !pre_existing {
                    let agents_dir = home_dir.join("agents");
                    if agents_dir.exists() {
                        match store.bootstrap_from_wiki(&agents_dir) {
                            Ok((inserted, skipped)) => info!(
                                inserted,
                                skipped, "Wiki trust store bootstrapped from frontmatter"
                            ),
                            Err(e) => warn!(error = %e, "Wiki trust bootstrap failed"),
                        }
                    }
                }

                // Phase 3 / R2-4: restart-aware daily janitor.
                // Reads `last_janitor_run_at` from the trust DB on boot;
                // fires immediately if more than a full interval has elapsed
                // since the last run, otherwise sleeps until the next 24-h
                // boundary. Persists the timestamp after every successful
                // pass so a crash-then-restart cycle never skips retention.
                let agents_dir = home_dir.join("agents");
                let janitor_store = store.clone();
                tokio::spawn(async move {
                    const INTERVAL: std::time::Duration = std::time::Duration::from_secs(24 * 3600);

                    let last_run = janitor_store
                        .meta_get("last_janitor_run_at")
                        .ok()
                        .flatten()
                        .and_then(|s| chrono::DateTime::parse_from_rfc3339(&s).ok())
                        .map(|d| d.with_timezone(&chrono::Utc));

                    // If we've never run OR more than one interval has passed,
                    // run immediately.
                    let initial_delay = match last_run {
                        Some(t) => {
                            let elapsed = chrono::Utc::now()
                                .signed_duration_since(t)
                                .to_std()
                                .unwrap_or(INTERVAL);
                            INTERVAL.saturating_sub(elapsed)
                        }
                        None => std::time::Duration::ZERO,
                    };
                    if !initial_delay.is_zero() {
                        tokio::time::sleep(initial_delay).await;
                    }

                    loop {
                        run_wiki_janitor_pass(&agents_dir, &janitor_store, &janitor_cfg);
                        let now_str = chrono::Utc::now().to_rfc3339();
                        if let Err(e) = janitor_store.meta_set("last_janitor_run_at", &now_str) {
                            warn!(error = %e, "failed to persist janitor last-run timestamp");
                        }
                        tokio::time::sleep(INTERVAL).await;
                    }
                });

                // Phase 7: federation transport — periodic export to peers.
                // Skipped silently when no peers configured.
                if !federation_cfg.peers.is_empty() {
                    crate::wiki_trust_federation::spawn_federation_pusher(
                        store.clone(),
                        federation_cfg,
                    );
                }
            }
            Err(e) => warn!(
                path = %trust_db.display(),
                error = %e,
                "Wiki trust store init failed — trust feedback disabled"
            ),
        }
    }

    // ── Initialize user database & JWT ───────────────────────
    let user_db_path = home_dir.join("users.db");
    let user_db = Arc::new(UserDb::new(&user_db_path).map_err(|e| {
        duduclaw_core::error::DuDuClawError::Gateway(format!(
            "Failed to initialize user database: {e}"
        ))
    })?);
    // Ensure a default admin exists on first run
    let bootstrap_password = match user_db.ensure_default_admin() {
        Ok(password) => password, // Some(..) on first run, None if an admin already existed
        Err(e) => {
            // C2 fix: fail hard if we can't create admin — don't silently continue
            return Err(duduclaw_core::error::DuDuClawError::Gateway(format!(
                "Failed to initialize user database: {e}"
            )));
        }
    };

    // WP3 (DESIGN-installer-settings-integration-2026-08.md §4): land an
    // installer-written pending-account.json, if the graphical installer ran
    // on this machine and left one. MUST run before the first-run print
    // block below — a successful land invalidates `bootstrap_password` (the
    // one-time password `ensure_default_admin` just generated no longer
    // matches the now-claimed row), so printing has to check the
    // POST-landing claim state, not just whether this was a first run.
    crate::pending_account::land_pending_account(&user_db, &home_dir);

    // Stage 2 (design doc §5 plan (b)): land an installer-written
    // pending-network.json the same way — the live installer only
    // COLLECTS Wi-Fi SSID/passphrase (it never scans/connects live, no
    // gateway payload ships in the live image), so the TARGET system's own
    // gateway is what actually joins the network, on its own first boot.
    // Runs in the background (bounded retries — Wi-Fi hardware/iwd may not
    // be ready this early) and never blocks startup; independent of the
    // account landing above, so ordering between the two doesn't matter.
    crate::pending_network::spawn_pending_network_landing(home_dir.clone());

    if bootstrap_password.is_some() && user_db.is_unclaimed_default_admin() {
        // First-run bootstrap, and still unclaimed after the landing attempt
        // above (no installer pending file existed, or landing it failed —
        // in both cases the original first-run guidance still applies). The
        // dashboard's first-open screen lets a LOOPBACK operator SET the
        // admin password directly (the `/api/first-run/claim` flow), so on
        // a localhost bind the generated one-time password is a stale
        // second path — printing it just confuses the setup. Only a
        // non-loopback bind (where the loopback-only claim endpoint is
        // unreachable from the operator's browser) still needs the printed
        // value to get in at all.
        let loopback_only = matches!(config.bind.as_str(), "127.0.0.1" | "::1" | "localhost");
        if loopback_only {
            println!(
                "\n  🔑 First-run setup: open the dashboard and set the admin password there (admin@local)."
            );
            println!();
        } else {
            println!("\n  🔑 First-run admin — log in with this, you'll be asked to change it:");
            println!("     Email:    admin@local");
            println!(
                "     Password: {}",
                bootstrap_password.as_deref().unwrap_or_default()
            );
            println!();
        }
    }
    let jwt_config = Arc::new(JwtConfig::load_or_generate(&home_dir).map_err(|e| {
        duduclaw_core::error::DuDuClawError::Gateway(format!("Failed to initialize JWT: {e}"))
    })?);
    info!("User authentication system initialized");

    // Initialize session manager
    let session_db_path = home_dir.join("sessions.db");
    let session_manager = Arc::new(
        crate::session::SessionManager::new(&session_db_path).map_err(|e| {
            duduclaw_core::error::DuDuClawError::Gateway(format!(
                "Failed to initialize session manager: {e}"
            ))
        })?,
    );

    // Start periodic session cleanup (every 6 hours, remove sessions older than 72 hours)
    {
        let sm = session_manager.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(6 * 3600));
            loop {
                interval.tick().await;
                match sm.cleanup_inactive(72).await {
                    Ok(n) if n > 0 => info!("Cleaned up {} inactive sessions", n),
                    Ok(_) => {}
                    Err(e) => warn!("Session cleanup error: {}", e),
                }
            }
        });
    }

    // ── Runtime model discovery: startup probe + 12h refresh ───
    // Replaces the old hard-coded cloud model list — probes each installed
    // CLI / API for its real available models and caches to
    // runtime_models.json. Failures keep the previous cache (marked fallback).
    crate::runtime_models::spawn_periodic_refresh(home_dir.clone());

    // ── Cost telemetry: periodic cleanup + adaptive routing ────
    {
        let hd = home_dir.clone();
        tokio::spawn(async move {
            // Wait 10 minutes before first check
            tokio::time::sleep(std::time::Duration::from_secs(600)).await;
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(3600));
            loop {
                interval.tick().await;
                crate::cost_telemetry::adaptive_routing_check(&hd).await;
            }
        });
    }

    // ── Initialize prediction engine (Phase 1) ────────────────
    // Embedding provider: None for now (Tier 2 vocabulary_novelty fallback).
    // When BGE-small-zh is available at ~/.duduclaw/models/embedding/bge-small-zh/,
    // pass Some(Arc::new(OnnxEmbeddingProvider::load(...))) here.
    let prediction_db_path = home_dir.join("prediction.db");
    let metacognition_path = home_dir.join("metacognition.json");
    let prediction_engine = Arc::new(crate::prediction::engine::PredictionEngine::new(
        prediction_db_path,
        Some(metacognition_path.clone()),
    ));
    info!("Prediction engine initialized (embedding: none, using vocabulary_novelty fallback)");

    // ── Initialize GVU loop (Phase 2) ────────────────────────
    let gvu_db_path = home_dir.join("evolution.db");
    let gvu_encryption_key = crate::config_crypto::load_keyfile_public(&home_dir);
    let gvu_loop = Arc::new(
        crate::gvu::loop_::GvuLoop::with_encryption(&gvu_db_path, gvu_encryption_key.as_ref())
            // Evolution alerts (escalate-to-human) and FYI records
            // (「採用經驗法則」) land in the Activity Feed + evolution events
            // instead of a log line nobody reads.
            .with_alert_sink(crate::gvu::loop_::GvuAlertSink {
                home_dir: home_dir.clone(),
                prediction_engine: prediction_engine.clone(),
            }),
    );
    info!("GVU evolution loop initialized (AEE playbook path)");

    // ── WP2.5: schedule the AEE settlement sweeper (30 min ticks) ─────────
    // Closes every committed AEE round whose observation window has elapsed,
    // settling its entries accept/rollback against their own eval cases.
    // (S11: replaces the removed `ObservationFinalizer`, whose other half
    // swept legacy SOUL.md observation windows that no longer exist.)
    {
        let sweeper_home = home_dir.clone();
        tokio::spawn(crate::gvu::aee::run_settlement_sweeper(
            sweeper_home,
            std::time::Duration::from_secs(1800),
        ));
        info!("AEE settlement sweeper scheduled — 30 min interval");
    }

    // ── WP0.5: GVU stagnation detector (30 min ticks, same cadence as
    // the AEE settlement sweeper) ───────────────────────────────────────
    // Diagnostic finding (TODO-evolution-v3-2026-08.md §0): GVU can loop
    // forever without ever landing a change, and nothing surfaced that fact
    // to a human — a production agent burned 20 GVU cycles with zero
    // applies before anyone noticed, from a manual DB inspection. This
    // sweeps every agent with GVU history and raises a de-duplicated
    // Activity Feed + evolution-event alert the first time it enters a
    // stagnant state.
    {
        let monitor = Arc::new(crate::gvu::stagnation::StagnationMonitor::new(
            &gvu_db_path,
            gvu_encryption_key,
            home_dir.join("agents"),
            home_dir.clone(),
            prediction_engine.clone(),
        ));
        tokio::spawn(monitor.run(std::time::Duration::from_secs(1800)));
        info!("GVU stagnation detector scheduled — 30 min interval");
    }

    // ── Channel-outage alerting ─────────────────────────────────────────
    // `channel_failures.jsonl` previously had no reverse notification path —
    // a channel that stayed connectable but stopped actually delivering
    // messages was only ever discoverable by an operator opening the
    // dashboard. Sweeps for the same-channel/threshold/window signal on a
    // tighter cadence than the 30-min GVU checks above: the alert window
    // itself is only 10 minutes, so a 30-min tick would routinely miss (or
    // badly delay) the very condition it exists to catch.
    {
        let monitor = Arc::new(crate::channel_alerts::ChannelAlertMonitor::new(
            home_dir.clone(),
        ));
        tokio::spawn(monitor.run(std::time::Duration::from_secs(120)));
        info!("Channel-outage alert monitor scheduled — 2 min interval");
    }

    // ── Notification governance (W2-4) ──────────────────────────────────
    // Quiet hours defer L1/L2 notifications into `notify_queue.jsonl`
    // (`crate::notify_governance`). Nothing else would ever take them out
    // again: the push that queued a notice at 23:00 has no reason to run at
    // 08:00, so the drainer is the only thing that closes the loop. Runs
    // unconditionally and costs one `exists()` per minute when no agent has
    // quiet hours configured (the default).
    {
        let drainer = Arc::new(crate::notify_governance::DeferredNotifyDrainer::new(
            home_dir.clone(),
        ));
        tokio::spawn(drainer.run(std::time::Duration::from_secs(60)));
        info!("Deferred-notification drainer scheduled — 1 min interval");
    }

    // CausalStore commits content-free source lifecycle notices atomically
    // with direct invalidation, erase, and expiry. Drain them on startup and
    // each minute; failed CCR writes remain pending for the next pass.
    tokio::spawn(crate::causal_ccr_outbox::run(
        home_dir.clone(),
        std::time::Duration::from_secs(60),
    ));
    info!("Causal CCR revocation outbox scheduled — 1 min interval");

    // Trusted local connector events share the causal source fence with the
    // CCR outbox. Retry their cross-store cascade on startup and each minute.
    tokio::spawn(crate::connector_lifecycle::run(
        home_dir.clone(),
        std::time::Duration::from_secs(60),
    ));
    info!("Trusted connector source lifecycle scheduled — 1 min interval");

    // Retained ticket bytes carry an explicit `retention_until`. Until now
    // nothing executed it on a schedule — only an operator pressing the scrub
    // endpoint, or someone happening to read an expired blob, so expired rows
    // outlived their own retention promise. Hourly is enough for a deadline
    // expressed in days; it skips entirely when `decisions.db` does not exist.
    tokio::spawn(crate::decision_store::run_ticket_retention_sweeper(
        home_dir.clone(),
        std::time::Duration::from_secs(3_600),
    ));
    info!("Decision ticket-source retention sweeper scheduled — 1 h interval");

    // X1 方案 1: the task board as the decision twin's first REAL data source.
    // Self-gating twice over — `[decision] task_board_shadow` (default false)
    // and the `[decision] enabled` kill switch — so a deployment that never
    // opts in pays one `config.toml` read per tick and writes nothing. A
    // failed period is audited and dropped; it never touches the gateway.
    tokio::spawn(crate::decision_task_board_shadow::run(
        home_dir.clone(),
        std::time::Duration::from_secs(600),
    ));
    info!(
        "Decision task-board shadow scheduled — 10 min tick (off unless [decision] task_board_shadow = true)"
    );

    // ── Human-takeover handback sweeper (W3-1, pattern D10) ─────────────
    // The pause itself expires at read time — every consumer compares `until`
    // against now — so this task exists purely to tell the conversation the
    // AI is back. Costs one `exists()` per minute when nobody has taken
    // anything over (the overwhelmingly common case).
    {
        let sweeper = crate::takeover::TakeoverSweeper::new(home_dir.clone());
        tokio::spawn(sweeper.run(std::time::Duration::from_secs(60)));
        info!("Human-takeover handback sweeper scheduled — 1 min interval");
    }

    // Daily digest (C8). Self-gating: the scheduler reads
    // `config.toml [notify] daily_digest` on every tick and returns
    // immediately when it is off (the default), so a deployment that never
    // opts in pays one file read per minute and sends nothing.
    {
        let digest = Arc::new(crate::notify_digest::DailyDigestScheduler::new(
            home_dir.clone(),
        ));
        tokio::spawn(digest.run(std::time::Duration::from_secs(60)));
        info!(
            "Daily-digest scheduler scheduled — 1 min interval (off unless [notify] daily_digest = true)"
        );
    }

    // Belief loop × goal contract gap 2 (design-market-belief-loop-2026-08.md
    // §3 「自主研究」): sweeps every agent every 5 minutes, self-gating on
    // per-agent `agent.toml [research] self_study` (off by default) — a
    // deployment where no agent opts in pays one `agents/` directory read
    // per tick and creates nothing.
    {
        let self_study = Arc::new(crate::self_study::SelfStudyScheduler::new(home_dir.clone()));
        tokio::spawn(self_study.run(std::time::Duration::from_secs(300)));
        info!(
            "Self-study scheduler scheduled — 5 min interval (off unless an agent sets [research] self_study = true)"
        );
    }

    // WP-G1: scheduled device backups. Self-gating: the scheduler reads
    // `config.toml [backup] schedule_enabled` on every tick and returns
    // immediately when it is off (the default) — a deployment that never
    // opts in pays one file read per tick and creates nothing. Not a cron
    // reimplementation — `interval_hours` is a single fixed cadence, so a
    // short `tokio::time::interval` tick that re-checks "is it due yet"
    // (same idiom as `notify_digest::DailyDigestScheduler`) is the whole
    // mechanism this needs.
    {
        let backup_sched = Arc::new(crate::backup_schedule::BackupScheduler::new(
            home_dir.clone(),
        ));
        tokio::spawn(backup_sched.run(std::time::Duration::from_secs(300)));
        info!(
            "Backup scheduler scheduled — 5 min tick (off unless [backup] schedule_enabled = true)"
        );
    }

    // Event broadcast channel for pushing real-time updates (e.g. channel status) to dashboard
    let (event_tx, _) = broadcast::channel::<String>(64);
    handler.set_event_tx(event_tx.clone()).await;
    // B5: give `dashboard_navigate::push_dashboard_navigate` a handle to the
    // SAME sender every `/ws` connection subscribes to, so any code path in
    // the gateway process (no `Handler`/`ReplyContext` needed) can route an
    // open dashboard tab to a specific page.
    crate::dashboard_navigate::init(event_tx.clone());

    // WP0.8 (R8, 2026-08-06): the MistakeNotebook Arc is built HERE — before
    // `reply_ctx` — rather than at its historical construction site further
    // down (the `shared_gvu_ctx` block, see "P1 (2026-05-09)" below), so both
    // consumers share one instance.
    //
    // Root cause of the zero-write bug: `ReplyContext::with_mistake_notebook`
    // had literally zero call sites in the whole workspace, so
    // `ctx.mistake_notebook` was permanently `None` and every
    // `if let Some(ref nb) = ctx.mistake_notebook` write path in
    // `channel_reply.rs` was dead code. The notebook is the sole input to the
    // Reflexion loop (F2a prompt injection + F2b rule consolidation), so a
    // permanently-empty notebook silently disabled both.
    let mistake_notebook = Arc::new(crate::gvu::mistake_notebook::MistakeNotebook::new(
        &home_dir.join("evolution.db"),
    ));

    // Start channel bots if configured
    let reply_ctx = Arc::new(
        crate::channel_reply::ReplyContext::new(
            handler.registry().clone(),
            home_dir.clone(),
            session_manager.clone(),
            handler.channel_status().clone(),
            event_tx.clone(),
        )
        .with_prediction_engine(prediction_engine.clone())
        .with_gvu_loop(gvu_loop.clone())
        .with_memory_db(home_dir.join("memory.db"))
        .with_mistake_notebook(mistake_notebook.clone())
        .with_redaction_manager(handler.get_redaction_manager().await),
    );
    // Inject reply context into handler for channel hot-start/stop
    handler.set_reply_ctx(reply_ctx.clone()).await;

    // Store background task handles for graceful shutdown (BE-L4)
    let mut bg_handles: Vec<tokio::task::JoinHandle<()>> = Vec::new();

    // ── OS security line P0 (C1 producer 乙): SecurityPosture poll loop ──
    // Independent of the autopilot engine's own enable/disable gate below —
    // posture monitoring + failsafe degradation is a core safety mechanism,
    // not an opt-in automation. Reuses the SAME `reply_ctx.failsafe` every
    // channel reply's L1 gate already reads, so a Red-driven `L2Restricted`
    // on the `__global__` scope takes effect immediately, everywhere. See
    // `posture_watch.rs` module docs for the full design + known limits.
    bg_handles.push(crate::posture_watch::spawn(
        home_dir.clone(),
        reply_ctx.failsafe.clone(),
    ));

    // ── Skill synthesis auto-run scheduler (W19-P1) ───────────────────────────
    // Makes conversation→skill extraction autonomous: runs the Rollout-to-Skill
    // pipeline on an interval instead of waiting for a manual `skill_synthesis_run`
    // MCP call. Off by default — enable via `config.toml [skill_synthesis]
    // auto_run = true` (still dry-run unless `dry_run = false`). The flag is
    // re-read each poll, so it can be toggled without a gateway restart.
    bg_handles.push(crate::skill_synthesis_pipeline::scheduler::spawn(
        home_dir.clone(),
    ));
    info!(
        "Skill synthesis auto-run scheduler started (gated by config [skill_synthesis] auto_run)"
    );

    // Validate default_agent before wiring channels — a dangling default_agent
    // is the root cause of channel "identity mixing" (wrong agent answers).
    crate::channel_reply::validate_default_agent(&home_dir, handler.registry()).await;

    // Start channel bots — per-agent where supported.
    //
    // Every starter below is awaited directly on the boot path, and several
    // make a network round-trip (getMe / token fetch). All their HTTP clients
    // carry a ≤35s request timeout, so the worst case is a bounded delay —
    // but a hang here silently delays EVERYTHING after it (heartbeat, cron,
    // tick sources). The per-stage `info!` markers make any such stall
    // visible in the log instead of reconstructing it from absence
    // (2026-08 LWM incident: the boot position could not be located because
    // no stage markers existed and the pro binary logged nothing at all).
    info!("boot: channel startup begin (telegram → slack → discord → webhooks)");
    for (label, h) in crate::telegram::start_telegram_bots(&home_dir, reply_ctx.clone()).await {
        handler.register_channel_handle(&label, h).await;
    }
    for (label, h) in crate::slack::start_slack_bots(&home_dir, reply_ctx.clone()).await {
        handler.register_channel_handle(&label, h).await;
    }
    for (label, h) in crate::discord::start_discord_bots(&home_dir, reply_ctx.clone()).await {
        handler.register_channel_handle(&label, h).await;
    }
    // Webhook channels (LINE, WhatsApp, Feishu, Google Chat, Teams, WeCom,
    // DingTalk) — global only for now. Per-agent webhook routing requires
    // multi-path routers (TODO-per-agent-channels.md)
    let line_router = crate::line::start_line_bot(&home_dir, reply_ctx.clone()).await;
    // WP-E2: box-side relay client — no-ops unless `[relay] enabled` resolves
    // true (default off; default on under DUDUCLAW_APPLIANCE=1). When active,
    // it feeds LINE webhooks received via `duduclaw-relay` into the exact
    // same verify+dispatch path `line_router` above mounts for direct HTTP.
    crate::relay_client::spawn_relay_client(&home_dir, reply_ctx.clone());
    let whatsapp_router =
        crate::whatsapp::start_whatsapp_webhook(&home_dir, reply_ctx.clone()).await;
    let feishu_router = crate::feishu::start_feishu_webhook(&home_dir, reply_ctx.clone()).await;
    let googlechat_router =
        crate::googlechat::start_googlechat_webhook(&home_dir, reply_ctx.clone()).await;
    let teams_router = crate::msteams::start_teams_webhook(&home_dir, reply_ctx.clone()).await;
    let wecom_router = crate::wecom::start_wecom_webhook(&home_dir, reply_ctx.clone()).await;
    let dingtalk_router =
        crate::dingtalk::start_dingtalk_webhook(&home_dir, reply_ctx.clone()).await;
    let webchat_ctx = reply_ctx.clone();
    info!("boot: channel startup done — starting schedulers");

    // Start unified heartbeat scheduler (per-agent: evolution + cron + monitoring)
    // Replaces the old start_evolution_timers — each agent's HeartbeatConfig
    // now drives meso/macro reflections at its own interval or cron schedule.
    //
    // BUG-3 fix: wire a SilenceBreakerEvent channel so silence detection in
    // the scheduler turns into a real `silence_breaker` evolution event in
    // prediction.db (gated by a 4h per-agent cool-down).
    let (silence_tx, silence_rx) =
        tokio::sync::mpsc::unbounded_channel::<duduclaw_agent::SilenceBreakerEvent>();
    let heartbeat = duduclaw_agent::heartbeat::start_heartbeat_scheduler_with(
        home_dir.clone(),
        handler.registry().clone(),
        Some(silence_tx),
    );
    handler.set_heartbeat(heartbeat).await;
    info!("Heartbeat scheduler started (per-agent evolution + monitoring)");

    // ── Night Engine (N1–N4 idle-time compute suite) ──
    // Runs its own idle-aware loop over the same agent registry: for each agent
    // with `[night_engine] enabled = true` that has been idle past its
    // threshold, fire a budget-bounded night pass (N3 schema induction + N4
    // recurrence-gated consolidation are live/deterministic; N1/N2 sleep-time +
    // prefetch call a real model via `night_llm::RotatedNightLlm` when the
    // global `config.toml [night] llm_enabled = true` — otherwise the
    // scheduler passes `None` and N1/N2 no-op exactly as before). Safe to
    // always spawn — disabled by default per agent.
    let _night_engine = crate::night_engine::spawn_night_engine(
        home_dir.clone(),
        handler.registry().clone(),
        300, // check every 5 minutes
    );
    info!("Night Engine scheduler started (idle-time N1–N4, disabled per-agent by default)");

    // ── Agent Mail (P2-d) ──
    // Inbound polling + the outbound confirmation settler. Always spawned, but
    // every pass is a no-op until `config.toml [mail] enabled = true`, so an
    // install that never configures a mailbox is byte-identical to before.
    // The config is re-read each tick, so switching it on needs no restart.
    let _mail_worker =
        crate::mail_worker::start_mail_worker(home_dir.clone(), handler.registry().clone());
    info!("Agent Mail worker started (no-op until [mail] enabled = true)");

    // ── Playbook stale/capacity sweep (WP1.2 G5) ──
    // Gateway-owned periodic loop, NOT hooked into
    // `duduclaw_agent::HeartbeatScheduler::run`'s tick body as the design doc
    // originally suggested: `duduclaw-agent` does not depend on
    // `duduclaw-gateway` (it's the reverse), so calling into
    // `crate::playbook` from that crate would introduce a dependency cycle.
    // This follows the exact same shape as `night_engine::spawn_night_engine`
    // just above — scan the shared registry on an interval, throttled
    // per-agent to 24h in-process. `run_decay`'s SQL excludes the semantic
    // layer, so playbook entries (semantic-layer rows) are never swept by it —
    // this loop is their only stale/capacity lifecycle driver.
    crate::playbook::spawn_playbook_sweep_loop(
        home_dir.clone(),
        handler.registry().clone(),
        3600, // check hourly; per-agent work only actually runs every 24h
    );
    info!("Playbook sweep loop started (stale/capacity lifecycle, G5)");

    // P1 (2026-05-09): build the GvuTriggerCtx once and share it across the
    // silence-event consumer and the dispatcher so both code paths fire GVU
    // through the same plumbing (loop / notebook / home dir). Constructed
    // before the silence consumer spawn — see #3.3 in
    // commercial/docs/TODO-runtime-health-fixes-202605.md for context.
    // WP0.8: `notebook` reuses the same Arc handed to `reply_ctx` above —
    // one notebook instance for the channel-reply write path and the
    // GVU-trigger read path.
    let shared_gvu_ctx = Arc::new(crate::prediction::subagent_prediction::GvuTriggerCtx {
        gvu_loop: gvu_loop.clone(),
        notebook: Some(mistake_notebook.clone()),
        home_dir: home_dir.clone(),
    });

    // Consume SilenceBreakerEvent → forced reflection event → optional GVU
    {
        let cooldown =
            Arc::new(crate::prediction::forced_reflection::SilenceBreakerCooldown::default_4h());
        crate::prediction::forced_reflection::spawn_silence_event_consumer(
            silence_rx,
            prediction_engine.clone(),
            cooldown,
            Some(shared_gvu_ctx.clone()),
        );
    }

    // ── Memory decay: archive old entries daily ───────────────
    // Archives entries older than 30 days (low-importance) and permanently
    // deletes archived entries older than 90 days.
    {
        let hd = home_dir.clone();
        tokio::spawn(async move {
            // Wait 5 minutes after startup before first run
            tokio::time::sleep(std::time::Duration::from_secs(300)).await;
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(24 * 3600));
            let policy = duduclaw_memory::decay::MemoryDecayPolicy {
                archive_after_days: 30,
                delete_after_days: 90,
                ..duduclaw_memory::decay::MemoryDecayPolicy::default()
            };
            loop {
                interval.tick().await;
                let db_path = hd.join("memory.db");
                let home_for_decay = hd.clone();
                let p = policy.clone();
                tokio::task::spawn_blocking(move || {
                    // H4: build through `memory_factory` so `[memory]
                    // novelty_gate` is honored on this path too (it used to
                    // call `SqliteMemoryEngine::new` directly, leaving the
                    // engine with no embedder).
                    let engine = match crate::memory_factory::build_memory_engine(
                        &db_path,
                        &home_for_decay,
                    ) {
                        Ok(e) => e,
                        Err(e) => {
                            tracing::warn!("Memory decay: failed to open memory.db: {e}");
                            return;
                        }
                    };
                    let rt = tokio::runtime::Handle::current();
                    rt.block_on(duduclaw_memory::decay::run_decay(&engine, &p));
                });
            }
        });
    }

    // Start cron scheduler (reads from SQLite cron_tasks.db, fires on schedule)
    let cron_store = Arc::new(crate::cron_store::CronStore::open(&home_dir).map_err(|e| {
        duduclaw_core::error::DuDuClawError::Gateway(format!("Failed to open cron store: {e}"))
    })?);
    handler.set_cron_store(cron_store.clone()).await;

    // Initialize task board store (SQLite tasks.db + activity feed)
    let task_store_opt: Option<Arc<crate::task_store::TaskStore>> =
        match crate::task_store::TaskStore::open(&home_dir) {
            Ok(ts) => {
                let arc = Arc::new(ts);
                handler.set_task_store(arc.clone()).await;
                // Share the same Arc with claude_runner so system-prompt
                // task injection reuses this connection rather than
                // opening a new SQLite handle per agent invocation.
                crate::claude_runner::set_shared_task_store(arc.clone());
                info!("Task board store initialized");
                Some(arc)
            }
            Err(e) => {
                warn!("Failed to open task store: {e}");
                None
            }
        };

    // Initialize autopilot rule store (SQLite autopilot.db)
    let autopilot_store_opt: Option<Arc<crate::autopilot_store::AutopilotStore>> =
        match crate::autopilot_store::AutopilotStore::open(&home_dir) {
            Ok(ap) => {
                let arc = Arc::new(ap);
                handler.set_autopilot_store(arc.clone()).await;
                info!("Autopilot store initialized");
                // OS security line P0 (C4): seed the default (disabled)
                // security autopilot rule pack once. Never blocks boot —
                // see `security_rules_seed.rs` for the full seed contract.
                let seed_report = crate::security_rules_seed::run(&home_dir, arc.as_ref()).await;
                if !seed_report.seeded.is_empty() {
                    info!(
                        seeded = ?seed_report.seeded,
                        "G19 security rules seed: default security autopilot rules created (disabled)"
                    );
                }
                Some(arc)
            }
            Err(e) => {
                warn!("Failed to open autopilot store: {e}");
                None
            }
        };

    let (cron_handle, cron_scheduler) = crate::cron_scheduler::start_cron_scheduler(
        home_dir.clone(),
        cron_store.clone(),
        handler.registry().clone(),
    );
    handler.set_cron_scheduler(cron_scheduler).await;
    bg_handles.push(cron_handle);
    info!("Cron scheduler started (SQLite-backed with hot reload)");

    // Account health probe — periodically tests unhealthy CLI accounts and restores
    // them by priority when they recover (e.g. rate-limit cooldown expired).
    {
        let probe_interval = std::fs::read_to_string(home_dir.join("config.toml"))
            .ok()
            .and_then(|s| s.parse::<toml::Table>().ok())
            .and_then(|t| {
                t.get("rotation")?
                    .as_table()?
                    .get("health_check_interval_seconds")?
                    .as_integer()
            })
            .unwrap_or(60) as u64;
        crate::claude_runner::spawn_health_probe(home_dir.clone(), probe_interval);
        info!(
            interval_secs = probe_interval,
            "Account health probe started"
        );
    }

    // Ensure every agent has a `.mcp.json` with the duduclaw MCP server entry.
    //
    // Claude CLI in `-p --dangerously-skip-permissions` mode does NOT read
    // global `~/.claude/settings.json` MCP servers — it only reads project-level
    // `.mcp.json` from the working directory. So per-agent `.mcp.json` is required.
    //
    // `ensure_duduclaw_absolute_path()` handles 3 cases:
    // 1. No `.mcp.json` → creates one with the resolved duduclaw binary
    // 2. Relative command → resolves to absolute path
    // 3. Non-existent binary (e.g., stale `duduclaw-pro`) → fixes it
    {
        let agents_dir = home_dir.join("agents");
        let fixed = duduclaw_agent::mcp_template::ensure_mcp_absolute_paths_all(&agents_dir);
        if fixed > 0 {
            info!(
                count = fixed,
                "Fixed/created .mcp.json for agent MCP server discovery"
            );
        }
    }

    // Initialize SQLite message queue (Phase 3 Hybrid TaskPipeline)
    let message_queue = match crate::message_queue::MessageQueue::open(&home_dir) {
        Ok(mq) => {
            info!("SQLite message queue initialized");
            Some(std::sync::Arc::new(mq))
        }
        Err(e) => {
            warn!("Failed to open SQLite message queue: {e} — falling back to JSONL only");
            None
        }
    };

    // Start agent dispatcher (consumes bus_queue.jsonl + SQLite queue, spawns sub-agents).
    // Clone the Arc so AutopilotEngine can share the same MessageQueue (delegate action).
    let mq_for_autopilot = message_queue.clone();
    // Clone for the P1 goal loop driver (spawned below alongside the dispatch
    // engine); `message_queue` itself is moved into the dispatcher.
    let mq_for_goal_loop = message_queue.clone();
    // Inject the queue into the handler so `system.update_config` can rebuild the
    // goal-loop driver on a hot config reload (iteration_cap_simple / policy).
    if let Some(mq) = mq_for_goal_loop.clone() {
        handler.set_message_queue(mq).await;
    }
    // P1 fix (2026-05-09): reuse the shared GvuTriggerCtx built earlier so
    // dispatcher + silence consumer share the same GvuLoop / MistakeNotebook
    // — keeps post-GVU bookkeeping consistent across the two trigger paths.
    bg_handles.push(crate::dispatcher::start_agent_dispatcher_with_crypto(
        home_dir.clone(),
        handler.registry().clone(),
        None,
        message_queue,
        Some(prediction_engine.clone()),
        Some(shared_gvu_ctx.clone()),
    ));
    info!(
        "Agent dispatcher started ({} background tasks)",
        bg_handles.len()
    );

    // ── Autopilot trigger engine (Multica-inspired event-driven automation) ──
    // Subscribes to a typed broadcast bus. Events come from:
    //   1) WebSocket handlers (in-process, via `set_autopilot_event_tx`)
    //   2) MCP subprocess (out-of-process) through the SQLite event bus
    //      at `events.db` — replaces the legacy `events.jsonl` file bus.
    //   3) G4: Odoo ERP changes (poller + `/webhook/odoo`), see below.
    // The Odoo webhook route is mounted with the rest of the router far below,
    // long after `ap_tx` has gone out of scope, so the sender is lifted out
    // here. `None` ⇒ no autopilot bus at all (no task/autopilot store), and
    // the route is simply not mounted — an ERP event with nowhere to go.
    let mut odoo_event_tx: Option<
        tokio::sync::broadcast::Sender<crate::autopilot_engine::AutopilotEvent>,
    > = None;
    if let (Some(ap_store), Some(ts)) = (autopilot_store_opt, task_store_opt.clone()) {
        // Capacity 8192: covers a burst of ~4000 events/hr without
        // dropping under a slow DB. Beyond this, `RecvError::Lagged`
        // surfaces in both the error log and the Activity Feed so the
        // drop isn't silent.
        let (ap_tx, ap_rx) =
            tokio::sync::broadcast::channel::<crate::autopilot_engine::AutopilotEvent>(8192);
        handler.set_autopilot_event_tx(ap_tx.clone()).await;
        // OS security line P0 (C1 producer 甲): give every gateway call site
        // (many have no `&self` on `MethodHandler`/`ReplyContext` to thread a
        // sender through) a process-global way to mirror a security audit
        // event onto this SAME bus. See `security_autopilot.rs` module docs.
        crate::security_autopilot::set_security_event_tx(ap_tx.clone());

        // ── OS-native per-edition quota (P4-3) ──────────────────────────────
        // Resolve, ONCE, which os_native agents may run OS-native features
        // under the edition quota (Personal = 1 seat). This single decision is
        // shared by all three init paths below so they agree on exactly which
        // agents are live (fail-closed consistency with the write-time gate —
        // both consult `license_runtime::os_native_agent_quota`). Over-quota
        // agents are warn-logged in the resolver and audited here.
        let os_native_quota =
            crate::license_runtime::os_native_agent_quota(handler.resolve_edition_profile().await);
        let os_allowed = crate::os_events::resolve_os_native_allowed(
            handler.registry().as_ref(),
            os_native_quota,
        )
        .await;
        for skipped in &os_allowed.skipped {
            crate::security_autopilot::audit_and_emit(
                &home_dir,
                &duduclaw_security::audit::AuditEvent::new(
                    "os_native_quota_skipped",
                    skipped,
                    duduclaw_security::audit::Severity::Warning,
                    serde_json::json!({
                        "quota": os_native_quota,
                        "reason": "os_native quota exceeded at startup; agent skipped",
                    }),
                ),
            );
        }
        let os_allowed_set = os_allowed.allowed;

        // ── OS-native Phase 1: filesystem watchers → autopilot bus ──────────
        // Populate the shared OsWatcherRegistry (held in the handler so
        // `agents.update` can hot stop/start a single agent's watcher) with one
        // watcher per quota-allowed `os_native` agent that declares `[os_watch]
        // paths`, then spawn the periodic stats writer for the
        // `os_watch_status` MCP tool. No-op when no agent opts in.
        let os_registry = handler.os_watchers();
        crate::os_events::init_os_watchers(
            os_registry.clone(),
            handler.registry().clone(),
            ap_tx.clone(),
            &os_allowed_set,
        )
        .await;
        bg_handles.push(crate::os_events::spawn_stats_writer(os_registry));

        // ── OS-native P2-4: frontmost app/window polling → autopilot bus ────
        // One low-frequency poll task per quota-allowed agent with `[os_watch]
        // frontmost_poll_secs > 0` (opt-in). Held in the handler's
        // OsFrontmostRegistry so `os.settings.update` can hot stop/start it
        // (P4-3). No-op when no agent opts in.
        crate::os_frontmost::init_frontmost_polling(
            handler.os_frontmost(),
            handler.registry().clone(),
            ap_tx.clone(),
            &os_allowed_set,
        )
        .await;

        // ── OS-native P4-4: digital-footprint memory distillation ───────────
        // Aggregates os_file/os_frontmost into per-agent daily stats and
        // distills them into temporal memory once a UTC day boundary is
        // crossed. Opt-in via `[os_watch] footprint = true`, additionally
        // layered on top of `os_native` + quota (deny-by-default at the write
        // AND the aggregation layer). The tracker is held in the handler so
        // `os.settings.update` can hot enable/disable an agent (P4-3); its two
        // background tasks are always armed for a later hot opt-in.
        bg_handles.extend(
            crate::footprint_distill::init_footprint_distill(
                handler.footprint_tracker(),
                handler.registry().clone(),
                ap_tx.clone(),
                &os_allowed_set,
            )
            .await,
        );

        // Poll SQLite event bus for events appended by MCP subprocesses.
        // Captured as `events_bus` (not dropped after this block) so the
        // P4-1 wiring below — the persistence bridge and the rule-induction
        // tick — can reuse the SAME `Arc<EventBusStore>` handle rather than
        // opening a second SQLite connection to the same file.
        let events_bus: Option<Arc<crate::events_store::EventBusStore>> =
            match crate::events_store::EventBusStore::open(&home_dir) {
                Ok(bus) => {
                    let bus = Arc::new(bus);
                    // WP6: the same tail also bridges channel-action feedback
                    // (`cron.changed` / `memory.changed` / `skill.changed`) to
                    // the dashboard WebSocket, so a routine created from
                    // Telegram appears on RoutinesPage without a reload.
                    bg_handles.push(crate::autopilot_engine::spawn_events_db_poll(
                        bus.clone(),
                        ap_tx.clone(),
                        Some(event_tx.clone()),
                    ));
                    info!("Event bus (events.db) poll task started");
                    Some(bus)
                }
                Err(e) => {
                    warn!(
                        "events.db open failed: {e} — MCP-originated events will not reach Autopilot"
                    );
                    None
                }
            };

        // ── P4-1: persist os_file/os_frontmost onto events.db ───────────────
        // Subscribes to the SAME broadcast the watchers/frontmost-poller above
        // feed. See `os_events::spawn_os_event_persistence` doc for why a
        // subscriber bridge (rather than a direct write in either forwarder)
        // and why its `source` marker is what keeps `spawn_events_db_poll`
        // above from re-dispatching the same event a second time. No-op
        // (nothing to persist to) when `events.db` failed to open.
        if let Some(bus) = events_bus.clone() {
            bg_handles.push(crate::os_events::spawn_os_event_persistence(
                bus,
                ap_tx.subscribe(),
            ));
        }

        // ── P4-1: PBD rule induction (30-minute tick) ───────────────────────
        // Closes the `rule_induction.rs` "known integration gap": now that
        // os_file/os_frontmost perception history lands in `events.db` (just
        // above), `RuleInductor` has rows to scan. Gated by its own
        // `config.toml [rule_induction] enabled` (default off — deny-safe;
        // see `RuleInductionConfig::from_home`), re-checked every tick. No-op
        // when `events.db` failed to open (nothing to scan).
        // (`.clone()`d rather than moved — the resident-sensing tick sources
        // below reuse the SAME `Arc<EventBusStore>` handle for their opt-in
        // `persist_every_n` audit trail.)
        if let Some(bus) = events_bus.clone() {
            bg_handles.push(crate::rule_induction::spawn_induction_loop(
                home_dir.clone(),
                bus,
                ap_store.clone(),
            ));
        }

        // One-shot cleanup of legacy file bus. Any in-flight events
        // during the upgrade window are lost; this is a one-time cost.
        let _ = tokio::fs::remove_file(home_dir.join("events.jsonl")).await;
        let _ = tokio::fs::remove_file(home_dir.join("events.jsonl.1")).await;

        // ── OS-native P2-1/P2-2: interruptibility tracker + ProactiveGate ───
        // The tracker ingests the SAME autopilot broadcast (os_frontmost /
        // os_file / agent_idle) to estimate cost-of-interruption; the gate reads
        // that score to raise its proactive threshold. Both are always
        // constructed — the gate only activates per-agent via `[proactive]
        // enabled = true` (deny-by-default), so wiring them unconditionally is
        // zero-cost for agents that never opt in.
        let interruptibility = Arc::new(crate::interruptibility::InterruptibilityTracker::new());
        bg_handles.push(interruptibility.clone().spawn(ap_tx.subscribe()));
        let proactive_gate = Arc::new(crate::proactive_gate::ProactiveGate::new(
            home_dir.clone(),
            interruptibility,
        ));

        // ── OS-native P2-3: outcome backfill + calibration loop ─────────────
        // Backfills `outcome` on due `proactive_gate.jsonl` lines and feeds the
        // False-Alarm / Missed-Need rate back into each opted-in agent's
        // base_threshold (see `proactive_feedback` module doc). Always
        // spawned — per-agent `[proactive] enabled` gates which agents it
        // calibrates, so this is zero-cost for agents that never opt in (same
        // rationale as the tracker/gate above).
        bg_handles.push(crate::proactive_feedback::spawn_feedback_loop(
            home_dir.clone(),
            session_manager.clone(),
            handler.registry().clone(),
        ));

        // ── P4-2: persona suppression rule induction ────────────────────
        // Aggregates false_alarm outcomes (the P2-3 backfill above) into
        // deterministic "when not to interrupt" persona rules. Independent
        // daily-gated loop — see `persona_induction` module doc "Cost: daily
        // tick". Same per-agent `[proactive] enabled` gate as the tracker/
        // gate/feedback loop above, so zero-cost for agents that never opt
        // in.
        bg_handles.push(crate::persona_induction::spawn_induction_loop(
            home_dir.clone(),
            handler.registry().clone(),
        ));

        // ── P3-3: lightweight CEP sequence matcher ──────────────────────
        // Subscribes to the SAME broadcast bus the engine consumes and
        // re-emits resolved `sequence` rule patterns as a synthetic
        // `AutopilotEvent::CepTrigger` onto that same bus — the engine's
        // `process_event` special-cases that variant so a resolved pattern
        // goes through the identical circuit-breaker / execute_action /
        // history tail as an ordinary single-event rule match. Purely
        // additive: rules without a `sequence` column are untouched.
        bg_handles.push(crate::cep_matcher::CepMatcher::spawn(
            ap_store.clone(),
            ap_tx.subscribe(),
            ap_tx.clone(),
        ));

        // ── Resident sensing: external data streams → autopilot bus ─────
        // One poll task per `config.toml [[tick.sources]]` entry, feeding
        // `AutopilotEvent::Tick` onto the SAME broadcast bus the engine and
        // the CEP matcher above consume, so a tick is matched by the exact
        // same deterministic rule machinery as every other event. Default
        // OFF (`[tick] enabled = false`): with no `[tick]` section
        // `active_sources()` is empty and not a single task is spawned.
        // The hub (recent-tick ring buffer) is created regardless so the
        // engine's wake-up context injection has a stable handle.
        let tick_hub = Arc::new(crate::tick_source::TickHub::new());
        // WP4: hand the dashboard/MCP handler the SAME hub the poll tasks
        // and the engine write into, so `ticks.sources`/`ticks.recent` read
        // live counters rather than a second, never-updated copy.
        handler.set_tick_hub(tick_hub.clone()).await;
        {
            let tick_cfg = crate::tick_config::TickConfig::from_home(&home_dir);
            let handles = crate::tick_source::spawn_tick_sources(
                &tick_cfg,
                &home_dir,
                ap_tx.clone(),
                tick_hub.clone(),
                events_bus.clone(),
            );
            if handles.is_empty() {
                info!("Resident sensing disabled (no active [tick] sources)");
            } else {
                info!(
                    sources = handles.len(),
                    "Resident sensing tick sources started"
                );
            }
            bg_handles.extend(handles);
        }

        // ── G4: Odoo ERP changes → autopilot bus ───────────────────────
        // `duduclaw-odoo`'s `PollTracker` / `classify_event` shipped with the
        // Odoo bridge but had no caller — the dashboard wrote `poll_enabled` /
        // `poll_models` into config.toml and nothing ever read them. This is
        // the missing consumer. Default OFF (`[odoo] poll_enabled = false`):
        // with no `[odoo]` section `spawn_odoo_poller` returns `None` and not
        // a single task is spawned. The webhook half is mounted further down
        // (it self-gates the same way `miniapp` does).
        if let Some(handle) = crate::odoo_events::spawn_odoo_poller(&home_dir, ap_tx.clone()) {
            bg_handles.push(handle);
        }
        odoo_event_tx = Some(ap_tx.clone());

        // Spawn the engine loop
        let engine = crate::autopilot_engine::AutopilotEngine::new(
            home_dir.clone(),
            ap_store,
            ts,
            mq_for_autopilot,
            ap_rx,
        )
        .with_proactive_gate(proactive_gate)
        .with_tick_hub(tick_hub);
        bg_handles.push(tokio::spawn(async move { engine.run().await }));
        info!("Autopilot trigger engine started");
    } else {
        info!("Autopilot engine disabled (missing task or autopilot store)");
        // OS-native Phase 1 watchers are only started inside the block above
        // (they forward onto the same broadcast bus the autopilot engine
        // consumes), so a missing task/autopilot store silently skips them too.
        // Warn explicitly when that's masking a real os_native config, so a
        // lean "no task board" deployment doesn't look like a silent bug.
        if crate::os_events::any_os_native_agents(handler.registry()).await {
            warn!(
                "os_native agent(s) configured but autopilot store/task store is not \
                 initialized — OS filesystem watchers were NOT started. Enable the task board / \
                 autopilot store to activate [os_watch]."
            );
        }
    }

    // ── Periodic update check (every 6 hours) — broadcast to dashboard ──
    // ── G1: durable dispatch engine ──────────────────────────
    // Background loop that provides the durability guarantees the legacy
    // bus_queue.jsonl file rail lacks: zombie reclaim (crashed-worker leases) +
    // goal-mode judge acceptance. Atomic claim / dependency unlock are enforced
    // in task_store and reached via the tasks_claim MCP tool.
    // The acceptance judge runs through the utility runtime choke-point
    // (`run_utility_prompt` → account rotator for Claude), so goal-mode `review`
    // tasks are evaluated on the same rotated LLM plumbing the fork/eval judges
    // use. Zombie reclaim + dependency gating are live regardless.
    // Default ON since v1.59 (see `dispatch_engine_enabled`; explicit
    // `[dispatch] enabled = false` opts out). Lease renewal is wired
    // (LeaseRenewalGuard for in-process workers, `tasks_renew` MCP heartbeat
    // for external agents) and reclaim is conservative (expiry + one full
    // unrenewed lease window). Synchronous claim/dependency/complete via the
    // MCP task tools work regardless of this flag.
    //
    // Build + spawn lives on the handler (self-gating on `[dispatch] enabled`)
    // so startup and the `system.update_config` hot reload share one path —
    // false→true first spawn, true→false teardown, both without a restart.
    // The engine respawn also owns constructing/registering the shared
    // forward-model `Arc` (gated on `[task_forward_model] enabled`); the goal
    // loop driver respawn AFTER it picks that same `Arc` up for its predict
    // hook, so both hooks share one coherent in-memory bucket cache (see
    // `MethodHandler::forward_model`'s doc comment).
    if handler.respawn_dispatch_engine().await {
        // ── P1: autonomous goal loop driver ──────────────────
        // The DispatchEngine only reviews goal-mode completions; it does NOT
        // drive execution. The goal loop driver is the missing outer loop:
        // it dispatches todo/pending goal_mode tasks onto the existing
        // message_queue wake-up rail, re-dispatches judge-rejected tasks with
        // feedback, and owns the hard termination guards.
        handler.respawn_goal_loop_driver().await;
    } else {
        info!(
            "Dispatch engine disabled ([dispatch] enabled = false；lease 續租仍接上，MCP task 工具不受影響)"
        );
    }

    // ── H6 (WP-B, `resume_on_restart`): boot-time-only reconciliation ──
    // Runs regardless of whether the dispatch engine just (re)started above
    // — a stale in-flight goal left over from a previous process must be
    // surfaced even if this particular boot happens to have dispatch
    // disabled, so it does not silently resume the next time dispatch is
    // re-enabled. No-op unless `[goal_loop] resume_on_restart = "pause"`.
    // Called exactly once, here, at boot — never from the
    // `system.update_config` hot-reload paths (see
    // `MethodHandler::pause_inflight_goal_tasks_on_restart`'s doc comment).
    let resumed_paused = handler.pause_inflight_goal_tasks_on_restart().await;
    if resumed_paused > 0 {
        info!(
            paused = resumed_paused,
            "resume_on_restart=pause: escalated in-flight goal tasks to needs_human at boot"
        );
    }

    // ── Maintenance Mode — Entry A: boot-time-only reassert-closed ────
    // `DESIGN-maintenance-mode-2026-08.md` §2.4: a gateway process restart
    // force-closes any in-flight maintenance window, unconditionally (even
    // with TTL time left) — stricter than `resume_on_restart=pause` above,
    // which still offers an `auto` mode; maintenance mode has no such
    // option at all. Called exactly once, here, at boot — never from a hot
    // reload path, mirroring `pause_inflight_goal_tasks_on_restart`'s own
    // contract. Runs unconditionally (no feature gate): a stale open window
    // surviving an in-memory-state wipe is exactly the orphan-window
    // scenario this call exists to close.
    if crate::maintenance::reassert_closed_on_boot(&home_dir).await > 0 {
        warn!(
            "maintenance mode was open before this restart — force-closed (revoke_reason=gateway_restart)"
        );
    }

    // ── D5: semi-automatic topology evolution (human-gated) ───
    // Independent of the dispatch engine: a slow background driver that mines
    // per-(agent, task_class) MAV reject / needs_human / oscillation evidence,
    // files reroute PROPOSALS (never direct changes) through the ApprovalBroker
    // as an always-human action, and auto-rolls-back approved overrides that do
    // not beat the baseline within the 24h observation window. Default OFF —
    // only runs when `[topology_evolution] enabled = true`. Build + spawn lives
    // on the handler (self-gating on `enabled`) so startup and the
    // `system.update_config` hot reload of `topology_evolution.enabled` share one
    // path (false→true first spawn, true→false teardown, both without a restart).
    handler.respawn_topology_driver().await;

    // Pro edition: auto-download + install + graceful restart (unless disabled).
    // CE edition: notify dashboard only.
    let auto_update = crate::updater::auto_update_enabled(&home_dir);
    {
        let etx = event_tx.clone();
        let home_for_update = home_dir.clone();
        // Update channel for this deployment. An extension-supplied provider
        // takes over both the check and the install; without one, a
        // `duduclaw-pro` wrapper can still SEE new versions (the Pro build
        // follows the OSS release train) but has nothing that can install them,
        // and CE stays on the public GitHub channel exactly as before.
        let update_provider = extension.update_provider();
        let update_channel = crate::updater::update_channel_label(update_provider.is_some());
        let notify_only_channel = update_channel == "none";
        tokio::spawn(async move {
            // First check after 30 seconds (let gateway finish startup)
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            // Last version we already logged an "unconfigured channel" skip for,
            // so an unattended Pro deployment logs once per release instead of
            // every 6h forever.
            let mut skip_logged_for: Option<String> = None;
            loop {
                let check_result = match &update_provider {
                    Some(provider) => provider.check().await,
                    None => crate::updater::check_update().await,
                };
                match check_result {
                    Ok(info) if info.available => {
                        let event = WsFrame::Event {
                            event: "system.update_available".to_string(),
                            payload: serde_json::json!({
                                "available": true,
                                "current_version": info.current_version,
                                "latest_version": info.latest_version,
                                "release_notes": info.release_notes,
                                "published_at": info.published_at,
                                "install_method": info.install_method,
                                "auto_update": auto_update,
                                "update_channel": update_channel,
                            }),
                            seq: None,
                            state_version: None,
                        };
                        if let Ok(json) = serde_json::to_string(&event) {
                            let _ = etx.send(json);
                        }

                        if auto_update && notify_only_channel {
                            // Pro wrapper with no update provider: installing the
                            // public asset is refused by design (it would replace
                            // the wrapper with the CE binary). Say so once per
                            // version — writing an `auto_update_failed` audit
                            // record every 6h against an intentional refusal is
                            // noise that buries real failures.
                            if skip_logged_for.as_deref() != Some(info.latest_version.as_str()) {
                                info!(
                                    latest = %info.latest_version,
                                    "Pro update channel not configured — skipping auto-install"
                                );
                                skip_logged_for = Some(info.latest_version.clone());
                            }
                        } else if auto_update {
                            // Pro auto-update: download, verify, install, restart
                            info!(
                                latest = %info.latest_version,
                                "Auto-update: downloading v{}...",
                                info.latest_version,
                            );

                            // Audit log
                            crate::security_autopilot::audit_and_emit(
                                &home_for_update,
                                &duduclaw_security::audit::AuditEvent::new(
                                    "auto_update_start",
                                    "system",
                                    duduclaw_security::audit::Severity::Info,
                                    serde_json::json!({
                                        "from": info.current_version,
                                        "to": info.latest_version,
                                    }),
                                ),
                            );

                            let apply_result = match &update_provider {
                                Some(provider) => provider.apply(&info, &|_| {}).await,
                                None => {
                                    crate::updater::apply_update(
                                        &info.download_url,
                                        &info.checksum_url,
                                    )
                                    .await
                                }
                            };
                            match apply_result {
                                Ok(result) if result.success => {
                                    info!("Auto-update installed v{}", info.latest_version);

                                    // Notify dashboard before restart
                                    let done_event = WsFrame::Event {
                                        event: "system.update_installed".to_string(),
                                        payload: serde_json::json!({
                                            "version": info.latest_version,
                                            "needs_restart": result.needs_restart,
                                            "message": result.message,
                                        }),
                                        seq: None,
                                        state_version: None,
                                    };
                                    if let Ok(json) = serde_json::to_string(&done_event) {
                                        let _ = etx.send(json);
                                    }

                                    crate::security_autopilot::audit_and_emit(
                                        &home_for_update,
                                        &duduclaw_security::audit::AuditEvent::new(
                                            "auto_update_success",
                                            "system",
                                            duduclaw_security::audit::Severity::Info,
                                            serde_json::json!({
                                                "version": info.latest_version,
                                                "needs_restart": result.needs_restart,
                                            }),
                                        ),
                                    );

                                    if result.needs_restart {
                                        // Graceful shutdown after 3s to let WebSocket
                                        // clients receive the notification. The
                                        // restart flag makes the post-shutdown hook
                                        // re-exec the new binary (works with or
                                        // without launchd/systemd supervision).
                                        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                                        info!(
                                            "Auto-update: restarting for v{}",
                                            info.latest_version
                                        );
                                        duduclaw_core::platform::request_restart_after_shutdown();
                                        duduclaw_core::platform::self_interrupt();
                                    }
                                }
                                Ok(result) => {
                                    // apply_update returned success=false (e.g. Homebrew)
                                    warn!(
                                        msg = %result.message,
                                        "Auto-update skipped"
                                    );
                                }
                                Err(e) => {
                                    warn!(error = %e, "Auto-update failed — will retry next cycle");

                                    crate::security_autopilot::audit_and_emit(
                                        &home_for_update,
                                        &duduclaw_security::audit::AuditEvent::new(
                                            "auto_update_failed",
                                            "system",
                                            duduclaw_security::audit::Severity::Warning,
                                            serde_json::json!({
                                                "target_version": info.latest_version,
                                                "error": e.replace('\n', " "),
                                            }),
                                        ),
                                    );
                                }
                            }
                        } else {
                            info!(
                                latest = %info.latest_version,
                                "New version available — notified dashboard clients"
                            );
                        }
                    }
                    Ok(_) => { /* up to date, no broadcast */ }
                    Err(e) => {
                        tracing::debug!(error = %e, "Periodic update check failed (will retry)");
                    }
                }
                // Check every 6 hours
                tokio::time::sleep(std::time::Duration::from_secs(6 * 3600)).await;
            }
        });
        info!(
            auto_update,
            update_channel,
            "Periodic update checker started (every 6h, auto_update={auto_update}, channel={update_channel})",
        );
    }

    // Start reminder scheduler (time-wheel based, 10s disk polling for cross-process pickup)
    bg_handles.push(crate::reminder_scheduler::start_reminder_scheduler(
        home_dir.clone(),
        handler.registry().clone(),
    ));
    info!("Reminder scheduler started");

    // #13 (2026-05-12): async session summarizer task.
    // Every 10 min, scan sessions that have ≥ 10 new turns since their
    // last summary (or never summarized) and run Haiku to fold the older
    // turns into a bullet summary. channel_reply reads this summary in
    // lieu of the verbatim slice, keeping the hot conversation context tight.
    bg_handles.push(crate::session_summarizer_task::spawn_summarizer(
        session_manager.clone(),
        home_dir.clone(),
        crate::session_summarizer::SummarizeParams::default(),
    ));
    info!("Session summarizer task started (10-min cadence)");

    // Session auto-titles (2026-07-29): every 10 min, give recently-active
    // sessions a short LLM title that follows the discussion (re-titled after
    // enough new turns). The WebChat conversation list prefers this over its
    // first-user-message fallback. Cost-guarded: 48h activity window + 5
    // titles/tick cap.
    bg_handles.push(crate::session_titler_task::spawn_titler(
        session_manager.clone(),
        home_dir.clone(),
        crate::session_titler_task::TitleParams::default(),
    ));
    info!("Session titler task started (10-min cadence)");

    // WP19 one-time migration (2026-08-04): backfill the bundled skills into
    // the company-wide layer for installs created before v1.51.1, where only
    // the MCP `create_agent` path seeded them — every other onboarding route
    // left the customer with a permanently blank Skills page. Marker-gated, so
    // a skill deleted on purpose afterwards stays deleted. Never blocks boot.
    {
        let report = crate::builtin_skills_seed_migration::run(&home_dir);
        if !report.seeded.is_empty() {
            info!(
                count = report.seeded.len(),
                skills = ?report.seeded,
                "WP19 built-in skills backfill applied"
            );
        }
    }

    // I-2b provenance backfill (2026-08-15): files archived before the
    // artifacts ledger existed carry no origin, so `/files` and the task
    // 「產物」tab would show them as history-less rows forever. The pass is
    // idempotent — a file that already has a row is skipped — so it runs every
    // boot and costs one directory listing per agent after the first time.
    // Attribution is evidence-only: what `task_changes.jsonl` can place gets a
    // task id, everything else is recorded honestly as 來源不明.
    {
        let report = crate::artifacts::backfill(&home_dir);
        if report.added() > 0 {
            info!(
                scanned = report.scanned,
                attributed = report.attributed,
                unknown = report.unknown,
                "I-2b artifact provenance backfill applied"
            );
        }
    }

    // Inject user_db into handler for user management RPC methods
    handler
        .set_user_db(user_db.clone(), jwt_config.clone())
        .await;

    let otp_delivery: Arc<dyn crate::otp_delivery::OtpDeliverer> = Arc::new(
        crate::otp_delivery::ConfigOtpDeliverer::new(home_dir.clone(), reqwest::Client::new()),
    );

    let state = Arc::new(AppState {
        auth: AuthManager::new(config.auth_token),
        handler,
        tx,
        event_tx,
        user_db,
        jwt_config,
        otp_delivery,
        home_dir: home_dir.clone(),
    });

    // M1/M60: open the shared audit index once and refresh it on a background
    // interval, so audit/reliability requests reuse one connection instead of
    // opening + full-syncing per request.
    {
        let bg_state = state.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(30));
            loop {
                tick.tick().await;
                bg_state.handler.refresh_audit_index().await;
            }
        });
    }

    // Edition live-watch: license transitions that do NOT flow through an RPC
    // (phone-home downgrade, CRL revocation, grace-period expiry) must still
    // reach open dashboards. The RPC paths (`license.activate` /
    // `license.redeem`) broadcast inline; this 60s poll is the safety net for
    // background transitions, broadcasting only on an actual change.
    {
        let bg_state = state.clone();
        tokio::spawn(async move {
            let mut last = bg_state.handler.resolve_edition_profile().await;
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(60));
            tick.tick().await; // consume the immediate first tick
            loop {
                tick.tick().await;
                let now = bg_state.handler.resolve_edition_profile().await;
                if now != last {
                    tracing::info!(
                        from = %last.as_str(),
                        to = %now.as_str(),
                        "edition changed in background — broadcasting system.status"
                    );
                    bg_state.handler.broadcast_system_status().await;
                    last = now;
                }
            }
        });
    }

    // WebChat endpoint — C5: now requires JWT auth (in-band) + Origin check,
    // mirroring the main /ws gate instead of being unauthenticated.
    let webchat_state = Arc::new(crate::webchat::WebChatState::new(
        webchat_ctx,
        state.jwt_config.clone(),
        state.user_db.clone(),
    ));
    let webchat_router = Router::new()
        .route("/ws/chat", get(crate::webchat::ws_chat_handler))
        .with_state(webchat_state);

    // ── REST API endpoints for authentication ────────────────
    let auth_router = Router::new()
        .route("/api/login", post(handle_login))
        .route("/api/otp/request", post(handle_otp_request))
        .route("/api/otp/verify", post(handle_otp_verify))
        .route("/api/channel-identity/bind", post(handle_channel_bind))
        .route(
            "/api/channel-identity/list",
            get(handle_channel_identity_list),
        )
        .route("/api/refresh", post(handle_refresh))
        .route("/api/me", get(handle_me))
        .route("/api/change-password", post(handle_change_password))
        .route("/api/first-run/status", get(handle_first_run_status))
        .route("/api/first-run/claim", post(handle_first_run_claim))
        // D4a: OOBE pre-auth network setup — see `first_run_network_gate`'s
        // doc for the fail-closed conditions shared by all three routes.
        // Deliberately no `/api/first-run/network/forget` — see
        // `handle_first_run_network_connect`'s doc.
        .route(
            "/api/first-run/network/status",
            get(handle_first_run_network_status),
        )
        .route(
            "/api/first-run/network/scan",
            post(handle_first_run_network_scan),
        )
        .route(
            "/api/first-run/network/connect",
            post(handle_first_run_network_connect),
        )
        .route("/api/session/local", post(handle_local_session))
        .with_state(state.clone());

    let mut app = Router::new()
        .route("/ws", get(ws_handler))
        .route("/health", get(health_handler))
        // `/healthz` — JSON liveness probe used by the desktop Gateway picker
        // (WP-GW) to validate a manually-entered / discovered gateway and show
        // its version + name before navigating. No auth (mirrors `/health`).
        .route("/healthz", get(healthz_handler))
        .route("/metrics", get(crate::metrics::metrics_handler))
        // Dashboard file panel (WP1.4): list + download an AI staff member's
        // attachment files. Bearer-JWT gated; download also accepts the JWT as
        // a `token` query param so browser preview/download links work.
        .route("/api/files", get(handle_files_list))
        .route("/api/files/download", get(handle_files_download))
        .route("/api/files/preview", get(handle_files_preview))
        .route("/api/ccr/dashboard", get(handle_ccr_dashboard))
        .route(
            "/api/ccr/replay",
            post(handle_ccr_replay).layer(DefaultBodyLimit::max(
                crate::ccr_replay::MAX_REPLAY_INPUT_BYTES,
            )),
        )
        .route("/api/decision/overview", get(handle_decision_overview))
        .route("/api/decision/catalog", get(handle_decision_catalog))
        .route(
            "/api/decision/import-pilot",
            post(handle_decision_import_pilot)
                .layer(DefaultBodyLimit::max(MAX_OPERATOR_IMPORT_BODY_BYTES)),
        )
        .route(
            "/api/decision/shadow-monitor",
            get(handle_decision_shadow_monitor),
        )
        .route(
            "/api/decision/engineering-validation",
            post(handle_decision_engineering_validation).layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .route(
            "/api/decision/forecast-validation",
            post(handle_decision_forecast_validation).layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .route(
            "/api/decision/model-candidate/create",
            post(handle_decision_model_candidate_create).layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .route(
            "/api/decision/model-candidate/load",
            post(handle_decision_model_candidate_load).layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .route(
            "/api/decision/model-candidate/compare",
            post(handle_decision_model_candidate_compare).layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .route(
            "/api/decision/model-candidate/load-run",
            post(handle_decision_model_candidate_load_run).layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .route(
            "/api/decision/model-candidate/score",
            post(handle_decision_model_candidate_score).layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .route(
            "/api/decision/model-candidate/load-score",
            post(handle_decision_model_candidate_load_score)
                .layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .route(
            "/api/decision/outcome-fit/create",
            post(handle_decision_outcome_fit_create).layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .route(
            "/api/decision/outcome-fit/load",
            post(handle_decision_outcome_fit_load).layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .route(
            "/api/decision/outcome-screen/save",
            post(handle_decision_outcome_screen_save).layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .route(
            "/api/decision/outcome-screen/load",
            post(handle_decision_outcome_screen_load).layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .route(
            "/api/decision/outcome-screen/review/request",
            post(handle_decision_outcome_screen_review_request)
                .layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .route(
            "/api/decision/outcome-screen/review/status",
            post(handle_decision_outcome_screen_review_status)
                .layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .route(
            "/api/decision/shadow-policy/create",
            post(handle_decision_shadow_policy_create).layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .route(
            "/api/decision/shadow-policy/load",
            post(handle_decision_shadow_policy_load).layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .route(
            "/api/decision/shadow-forecast/create",
            post(handle_decision_shadow_forecast_create).layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .route(
            "/api/decision/shadow-forecast/load",
            post(handle_decision_shadow_forecast_load).layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .route(
            "/api/decision/shadow-score/create",
            post(handle_decision_shadow_score_create).layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .route(
            "/api/decision/shadow-score/load",
            post(handle_decision_shadow_score_load).layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .route(
            "/api/decision/shadow-policy/assess",
            post(handle_decision_shadow_policy_assess).layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .route(
            "/api/decision/shadow-screen/evaluate",
            post(handle_decision_shadow_screen_evaluate).layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .route(
            "/api/decision/shadow-screen/save",
            post(handle_decision_shadow_screen_save).layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .route(
            "/api/decision/shadow-screen/load",
            post(handle_decision_shadow_screen_load).layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .route(
            "/api/decision/shadow-screen/review/request",
            post(handle_decision_shadow_screen_review_request)
                .layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .route(
            "/api/decision/shadow-screen/review/status",
            post(handle_decision_shadow_screen_review_status)
                .layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .route(
            "/api/decision/shadow-sla-forecast/create",
            post(handle_decision_shadow_sla_forecast_create)
                .layer(DefaultBodyLimit::max(MAX_SHADOW_SLA_SOURCE_BODY_BYTES)),
        )
        .route(
            "/api/decision/shadow-sla-forecast/load",
            post(handle_decision_shadow_sla_forecast_load).layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .route(
            "/api/decision/shadow-sla-score/create",
            post(handle_decision_shadow_sla_score_create)
                .layer(DefaultBodyLimit::max(MAX_SHADOW_SLA_SOURCE_BODY_BYTES)),
        )
        .route(
            "/api/decision/shadow-sla-score/load",
            post(handle_decision_shadow_sla_score_load).layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .route(
            "/api/decision/shadow-sla-score/load-current",
            post(handle_decision_shadow_sla_score_load_current)
                .layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .route(
            "/api/decision/shadow-sla-policy/assess",
            post(handle_decision_shadow_sla_policy_assess).layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .route(
            "/api/decision/shadow-sla-screen/evaluate",
            post(handle_decision_sla_shadow_screen_evaluate).layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .route(
            "/api/decision/shadow-sla-screen/save",
            post(handle_decision_sla_shadow_screen_save).layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .route(
            "/api/decision/shadow-sla-screen/load",
            post(handle_decision_sla_shadow_screen_load).layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .route(
            "/api/decision/shadow-sla-screen/review/request",
            post(handle_decision_sla_shadow_screen_review_request)
                .layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .route(
            "/api/decision/shadow-sla-screen/review/status",
            post(handle_decision_sla_shadow_screen_review_status)
                .layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .route(
            "/api/decision/empirical-resampling",
            post(handle_decision_empirical_resampling).layer(DefaultBodyLimit::max(32 * 1024)),
        )
        .route(
            "/api/decision/synthetic-pilot",
            post(handle_decision_synthetic_pilot).layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .route(
            "/api/decision/synthetic-pilot/lifecycle",
            post(handle_decision_synthetic_pilot_lifecycle).layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .route(
            "/api/decision/compare",
            post(handle_decision_compare).layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .route(
            "/api/decision/event-compare",
            post(handle_decision_event_compare).layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .route(
            "/api/decision/replay",
            post(handle_decision_replay).layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .route(
            "/api/decision/pilot-review/request",
            post(handle_decision_pilot_review_request).layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .route(
            "/api/decision/pilot-review/status",
            post(handle_decision_pilot_review_status).layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .route(
            "/api/decision/policy-sweep",
            post(handle_decision_policy_sweep).layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .route(
            "/api/decision/sensitivity",
            post(handle_decision_sensitivity).layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .route(
            "/api/decision/ticket-sources/scrub",
            post(handle_decision_ticket_sources_scrub).layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .route(
            "/api/decision/task-board-export",
            post(handle_decision_task_board_export).layer(DefaultBodyLimit::max(4 * 1024)),
        )
        .route(
            "/api/decision/odoo-export",
            post(handle_decision_odoo_export).layer(DefaultBodyLimit::max(4 * 1024)),
        )
        .route("/api/causal/claims", get(handle_causal_claims))
        .route("/api/causal/claim", get(handle_causal_claim))
        .route("/api/causal/source", get(handle_causal_source))
        .route(
            "/api/causal/source/invalidate",
            post(handle_causal_source_invalidate).layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .route(
            "/api/causal/source/erase",
            post(handle_causal_source_erase).layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .route(
            "/api/causal/source/clear-revocation-fence",
            post(handle_causal_clear_revocation_fence).layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .route(
            "/api/causal/extract",
            post(handle_causal_extract).layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .route(
            "/api/causal/eval/extraction",
            post(handle_causal_extraction_eval).layer(DefaultBodyLimit::max(2 * 1024 * 1024)),
        )
        .route(
            "/api/causal/source/import-memory",
            post(handle_causal_import_memory).layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .route(
            "/api/causal/source/import-wiki",
            post(handle_causal_import_wiki).layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .route(
            "/api/causal/source/import-shared-wiki",
            post(handle_causal_import_shared_wiki).layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .route("/api/causal/models", get(handle_causal_models))
        .route("/api/causal/model", get(handle_causal_model))
        .route(
            "/api/causal/negative-control/protocol",
            post(handle_causal_negative_control_protocol).layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .route(
            "/api/causal/negative-control/review",
            get(handle_causal_negative_control_review_get)
                .post(handle_causal_negative_control_review)
                .layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .route(
            "/api/causal/effect/estimate",
            post(handle_causal_effect_estimate).layer(DefaultBodyLimit::max(2 * 1024 * 1024)),
        )
        .route("/api/causal/effect", get(handle_causal_effect))
        .route("/api/causal/aliases", get(handle_causal_aliases))
        .route(
            "/api/causal/claim/review",
            post(handle_causal_claim_review).layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .route(
            "/api/causal/claim/revise",
            post(handle_causal_claim_revise).layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .route(
            "/api/causal/model/review",
            post(handle_causal_model_review).layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .route(
            "/api/causal/model/assumptions",
            get(handle_causal_assumptions)
                .post(handle_causal_assumption_review)
                .layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .route(
            "/api/causal/alias",
            post(handle_causal_alias_set).layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .route(
            "/api/causal/alias/revoke",
            post(handle_causal_alias_revoke).layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .route("/api/mcp/oauth/callback", get(handle_mcp_oauth_callback))
        .route(
            "/api/reliability/summary",
            get(handle_reliability_summary_http),
        )
        // Voice endpoints (openhuman-parity B): STT (multipart audio → text) +
        // TTS (text → audio). Bearer-JWT gated. STT gets a raised body limit so
        // a short voice clip (≤10 MiB) is accepted; the default axum 2 MiB cap
        // would 413 most recordings.
        .route(
            "/api/stt",
            post(handle_stt).layer(DefaultBodyLimit::max(STT_MAX_UPLOAD_BYTES + 512 * 1024)),
        )
        // Expert-pack upload (dashboard 專家包 install flow). Bearer-JWT +
        // admin-role gated; 50 MiB cap (matches the safe_zip extraction cap).
        // Stages the zip under <home>/tmp/expert-uploads/ and returns the
        // server-local path for a follow-up `experts.install` RPC.
        .route(
            "/api/experts/upload",
            post(handle_expert_upload).layer(DefaultBodyLimit::max(
                crate::expert_admin::MAX_EXPERT_UPLOAD_BYTES + 512 * 1024,
            )),
        )
        // WP-G1: device-migration ("汰機搬家") restore upload + the
        // dedicated scheduled-backup download route. Admin + appliance JWT
        // gated (`authorize_device_admin`, mirrors `authorize_file_access`
        // but additionally requires appliance mode — the whole `device.*`
        // surface is appliance-only). Backups never share the attachments
        // download route — see `backup_schedule.rs`'s module doc.
        .route(
            "/api/device/backup-upload",
            post(handle_device_backup_upload).layer(DefaultBodyLimit::max(
                crate::backup_restore::MAX_BACKUP_UPLOAD_BYTES + 4 * 1024 * 1024,
            )),
        )
        .route(
            "/api/device/backups/download",
            get(handle_device_backup_download),
        )
        .route("/api/tts", post(handle_tts))
        .route(
            "/api/voice/config",
            get(handle_voice_config_get).post(handle_voice_config_set),
        )
        .with_state(state)
        .merge(auth_router)
        .merge(webchat_router);

    // Wiki trust federation inbound endpoint — only mounted when the trust
    // store is initialised. Fails closed by returning 503 from a stub when
    // not initialised, so peers get a clear error instead of a 404.
    //
    // CRITICAL (review C2): the federation route lives outside auth_router
    // (peers don't have user JWTs), so it must enforce its own body size
    // limit. 1 MiB caps the JSON body well before any reasonable batch
    // bumps against MAX_FEDERATION_UPDATES_PER_PUSH (5k × ~150 bytes).
    if let Some(store) = duduclaw_memory::trust_store::global_trust_store() {
        let federation_state = crate::wiki_trust_federation::FederationServerState {
            store,
            shared_secret: {
                let raw = std::fs::read_to_string(home_dir.join("config.toml")).unwrap_or_default();
                let table: toml::Table = raw.parse().unwrap_or_default();
                crate::wiki_trust_federation::FederationConfig::from_toml(&table).shared_secret
            },
        };
        app = app.merge(
            Router::new()
                .route(
                    "/api/v1/wiki_trust/federation",
                    post(crate::wiki_trust_federation::handle_federation_push)
                        .layer(axum::extract::DefaultBodyLimit::max(1024 * 1024)),
                )
                .with_state(federation_state),
        );
    }

    // ── License control-plane (P2, white-label owner) ─────────────
    // Always mounted; each handler self-gates on `[distributor] issuer_key_path`
    // (absent ⇒ 404) so a plain gateway exposes no behaviour. Public (no bearer)
    // — trust is proven by subscription_id + machine_fingerprint. Own state
    // (home_dir) + 64 KiB body cap, like the federation route above.
    app = app.merge(crate::license_serve::router(home_dir.clone()));

    // ── Telegram Mini App (D-S1 spike) ────────────────────────────
    // Always mounted; every handler self-gates on `config.toml [miniapp]
    // enabled` (default false) and 404s while off, so a stock install exposes
    // nothing. Public by construction — the caller proves identity with
    // Telegram-signed `initData`, not a dashboard JWT, and decisions are
    // routed through the same `decision_notify::route_press` a button press
    // uses. Own state (home_dir) + its own body cap, like the routes above.
    app = app.merge(crate::miniapp::router(home_dir.clone()));

    // ── G4: Odoo ERP webhook (`POST /webhook/odoo`) ───────────────
    // Same posture as the Mini App above: mounted whenever there is an
    // autopilot bus to deliver onto, and the handler self-gates on
    // `config.toml [odoo] webhook_enabled` (default false) — 404 while off,
    // 401 on a missing/wrong shared secret, and an *empty* configured secret
    // refuses everything rather than accepting anything.
    if let Some(tx) = odoo_event_tx.clone() {
        app = app.merge(crate::odoo_events::router(home_dir.clone(), tx));
    }

    // ── .well-known endpoints for protocol discovery ──────────────
    app = app
        .route(
            "/.well-known/mcp-server.json",
            get(well_known_mcp_server_card),
        )
        // A2A v1.0 signed Agent Card (G6). `agent-card.json` is the v1.0 path;
        // `agent.json` is kept as a legacy alias. Both serve the signed card.
        .route("/.well-known/agent-card.json", get(well_known_agent_card))
        .route("/.well-known/agent.json", get(well_known_agent_card))
        // JWKS advertising the A2A signing public key for card verification.
        .route("/.well-known/jwks.json", get(well_known_jwks));

    // Mount LINE webhook endpoint (always — the handler reads config per request)
    app = app.merge(line_router);
    // Mount configured webhook channels (each returns None when unconfigured)
    if let Some(r) = whatsapp_router {
        app = app.merge(r);
    }
    if let Some(r) = feishu_router {
        app = app.merge(r);
    }
    if let Some(r) = googlechat_router {
        app = app.merge(r);
    }
    if let Some(r) = teams_router {
        app = app.merge(r);
    }
    if let Some(r) = wecom_router {
        app = app.merge(r);
    }
    if let Some(r) = dingtalk_router {
        app = app.merge(r);
    }

    // Merge plugin extension routes (if any)
    if let Some(extra) = extension.extra_routes() {
        app = app.merge(extra);
    }

    #[cfg(feature = "dashboard")]
    {
        app = app.merge(duduclaw_dashboard::dashboard_router());
    }

    // ── Decision Lab kill switch (audit X1, 方案 6) ────────────────
    // `config.toml [decision] enabled` (default true — the line stays mounted).
    // Off ⇒ every `/api/decision/*` request 404s before routing, so an operator
    // can retire the still-exploratory decision-twin surface without a rebuild.
    // Resolved once at boot; a change takes effect on the next restart, like
    // the bind address. Neighbouring surfaces (`/api/causal/*`, `/api/ccr/*`)
    // keep their own switches and are deliberately untouched here.
    let decision_enabled = crate::decision_gate::DecisionConfig::from_home(&home_dir).enabled;
    if !decision_enabled {
        info!("Decision Lab surface disabled by config.toml [decision] enabled = false");
    }
    let app = app.layer(axum::middleware::from_fn_with_state(
        decision_enabled,
        crate::decision_gate::decision_surface_gate,
    ));

    let addr = format!("{}:{}", config.bind, config.port);
    info!("boot: all background subsystems wired — binding HTTP");
    info!("Gateway starting on {}", addr);

    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .map_err(|e| duduclaw_core::error::DuDuClawError::Gateway(e.to_string()))?;

    // WP-B: systemd sd_notify — tell systemd startup is complete, and start
    // watchdog pings if this unit is `Type=notify` with `WatchdogSec=` set.
    // Both are safe unconditional no-ops off-systemd (gated internally on
    // `$NOTIFY_SOCKET`/`$WATCHDOG_USEC`, not on `is_appliance()` — see
    // `watchdog.rs`'s module doc for why).
    if let Err(e) = crate::watchdog::notify_ready() {
        warn!("sd_notify READY=1 failed (non-fatal): {e}");
    }
    let _watchdog_pings = crate::watchdog::spawn_watchdog_pings();

    // LAN discovery: advertise this gateway over mDNS so desktop apps on the
    // same network can find it (WP-GW). Strictly best-effort — a failure only
    // warns and never blocks serving. Held for the lifetime of the process and
    // torn down (unregistered) inside the graceful-shutdown future below.
    let mdns_advertiser = {
        let host_os = hostname::get()
            .ok()
            .and_then(|h| h.into_string().ok())
            .filter(|h| !h.trim().is_empty())
            .unwrap_or_else(|| "duduclaw".to_string());
        let cfg_text = std::fs::read_to_string(home_dir.join("config.toml")).unwrap_or_default();
        let mdns_cfg = crate::mdns::MdnsConfig::from_toml_str(&cfg_text, &host_os);
        // Env override (`DUDUCLAW_MDNS_ADVERTISE`) wins over config — desktop-app
        // sidecars inject `=0` so an employee laptop never advertises (§2.5).
        let env_override = std::env::var(crate::mdns::MDNS_ADVERTISE_ENV).ok();
        let advertise = crate::mdns::resolve_advertise(mdns_cfg.advertise, env_override.as_deref());
        if advertise {
            match crate::mdns::MdnsAdvertiser::start(
                &mdns_cfg,
                &host_os,
                config.port,
                env!("CARGO_PKG_VERSION"),
            ) {
                Ok(adv) => {
                    info!(
                        service = %adv.fullname(),
                        name = %mdns_cfg.name,
                        "mDNS advertising enabled ({})",
                        crate::mdns::SERVICE_TYPE
                    );
                    Some(adv)
                }
                Err(e) => {
                    warn!("mDNS advertising disabled (register failed): {e}");
                    None
                }
            }
        } else {
            info!(
                "mDNS advertising disabled ([server] mdns_advertise defaults off; \
                 set = true to broadcast, or DUDUCLAW_MDNS_ADVERTISE env to override)"
            );
            None
        }
    };

    // Serve with graceful shutdown on Ctrl+C.
    //
    // Shutdown order: ctrl_c → prediction engine flush → axum drains →
    // main exits.
    let pe_for_shutdown = prediction_engine.clone();
    let meta_path_for_shutdown = metacognition_path.clone();
    // Hard deadline for the post-flush connection drain. axum's graceful
    // shutdown waits for EVERY in-flight connection — and dashboard
    // WebSocket / SSE / WebChat connections are long-lived and never close
    // on their own, so an unbounded drain wedges the process forever:
    // listener closed (requests time out) but the PID stays alive and the
    // self-update re-exec below is never reached (2026-08-03 field report:
    // dashboard update → gateway stuck, PID alive, port dead).
    const DRAIN_TIMEOUT_SECS: u64 = 10;
    /// Bound one shutdown step; a wedged step must not block the restart.
    async fn bounded_step<F: std::future::Future>(name: &str, secs: u64, fut: F) {
        if tokio::time::timeout(std::time::Duration::from_secs(secs), fut)
            .await
            .is_err()
        {
            warn!("{name} did not finish within {secs}s — continuing shutdown");
        }
    }
    let (drain_started_tx, drain_started_rx) = tokio::sync::oneshot::channel::<()>();
    let serve = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        let _ = tokio::signal::ctrl_c().await;
        info!("Shutdown signal received, flushing state...");
        // sd_notify STOPPING=1 — best-effort, no-op off-systemd.
        let _ = crate::watchdog::notify_stopping();
        // Withdraw the LAN advertisement first so peers stop offering a
        // gateway that is going away (sends the mDNS goodbye packet).
        if let Some(adv) = mdns_advertiser {
            info!("Withdrawing mDNS advertisement...");
            adv.stop();
        }
        bounded_step("prediction engine flush", 20, pe_for_shutdown.flush_all()).await;
        bounded_step(
            "metacognition persist",
            10,
            pe_for_shutdown.persist_metacognition(&meta_path_for_shutdown),
        )
        .await;
        info!("Prediction engine state flushed");
        // W3-4: the UCCI LocalStrong shadow generation runs detached so it
        // never blocks a reply, so one can still be in flight here — and its
        // calibration row would be lost on exit. Bounded tightly (a shadow is
        // a local generation, not a cloud round-trip) and warn-only: a slow
        // shadow must never hold up the restart.
        bounded_step(
            "UCCI shadow observation flush",
            5,
            crate::claude_runner::flush_inference_shadow_observations(),
        )
        .await;
        // Flush chain done — axum starts draining connections. Arm the
        // drain watchdog below.
        let _ = drain_started_tx.send(());
    });
    tokio::select! {
        r = serve => {
            r.map_err(|e| duduclaw_core::error::DuDuClawError::Gateway(e.to_string()))?;
        }
        // Only ever fires after the flush chain completed AND the drain has
        // been running for DRAIN_TIMEOUT_SECS (long-lived WS/SSE clients
        // never hang up, so waiting longer is pointless). Dropping the serve
        // future closes the remaining connections abruptly — by design.
        _ = async {
            // A dropped sender (shutdown task panicked/cancelled) is NOT the
            // drain starting — park forever rather than arming the watchdog and
            // tearing down live connections while nothing is shutting down.
            if drain_started_rx.await.is_err() {
                std::future::pending::<()>().await;
            }
            tokio::time::sleep(std::time::Duration::from_secs(DRAIN_TIMEOUT_SECS)).await;
        } => {
            warn!(
                "Connection drain exceeded {DRAIN_TIMEOUT_SECS}s (long-lived WebSocket/SSE \
                 clients) — forcing shutdown so restart/re-exec can proceed"
            );
        }
    }

    // Self-update installed a new binary during this run: re-exec into it
    // now that the graceful shutdown sequence (prediction flush → worker
    // supervisor SIGTERM chain → axum drain) has completed. exec() keeps
    // the PID on Unix, so launchd/systemd supervision is undisturbed; it
    // also covers unsupervised foreground runs (npm wrapper, `duduclaw run`).
    if duduclaw_core::platform::restart_requested() {
        info!("Update installed — re-executing new binary...");
        let err = duduclaw_core::platform::self_restart();
        // self_restart only returns on failure.
        tracing::error!(
            error = %err,
            "Self-restart failed — exiting; if running under launchd/systemd the supervisor will relaunch"
        );
    }

    Ok(())
}

// ── REST Auth Handlers ───────────────────────────────────────

#[derive(serde::Deserialize)]
struct LoginRequest {
    email: String,
    password: String,
}

#[derive(serde::Deserialize)]
struct RefreshRequest {
    refresh_token: String,
}

/// POST /api/login — Authenticate with email + password, return JWT tokens.
async fn handle_login(
    State(state): State<Arc<AppState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(body): Json<LoginRequest>,
) -> impl IntoResponse {
    let ip = addr.ip();
    // Rate limit login attempts — M2: scoped by (IP, email).
    if !check_login_rate_limit(ip, &body.email) {
        return (
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            Json(serde_json::json!({"error": "too many login attempts, try again in 15 minutes"})),
        )
            .into_response();
    }

    // Verify credentials
    let user = match state.user_db.verify_password(&body.email, &body.password) {
        Ok(u) => u,
        Err(e) => {
            warn!(email = %body.email, "Login failed: {e}");
            // M16: record failed logins so brute force is auditable. We log the
            // attempted email + source IP under the dedicated `login_failed`
            // action; user_id is unknown/untrusted so it stays NULL.
            let ip_str = ip.to_string();
            let _ = state.user_db.log_action(
                None,
                "login_failed",
                Some(&body.email),
                None,
                Some(&ip_str),
            );
            return (
                axum::http::StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"error": "invalid email or password"})),
            )
                .into_response();
        }
    };

    // Get agent bindings for this user
    let bindings = state.user_db.get_user_agents(&user.id).unwrap_or_default();
    let agent_access: Vec<(String, duduclaw_auth::AccessLevel)> = bindings
        .iter()
        .map(|b| (b.agent_name.clone(), b.access_level))
        .collect();

    // Issue tokens
    let access_token = match state.jwt_config.issue_access_token(&user, &agent_access) {
        Ok(t) => t,
        Err(e) => {
            error!("Failed to issue access token: {e}");
            return (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "token generation failed"})),
            )
                .into_response();
        }
    };

    let refresh_token = match state.jwt_config.issue_refresh_token(&user.id) {
        Ok(t) => t,
        Err(e) => {
            error!("Failed to issue refresh token: {e}");
            return (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "token generation failed"})),
            )
                .into_response();
        }
    };

    // M2: clear the failed-attempt counter on success so legitimate users are
    // not penalised by earlier typos and an attacker cannot lock the account.
    reset_login_rate_limit(ip, &body.email);

    // Update last login
    let _ = state.user_db.update_last_login(&user.id);

    // Audit log
    let ip_str = ip.to_string();
    let _ = state
        .user_db
        .log_action(Some(&user.id), "login", None, None, Some(&ip_str));

    Json(serde_json::json!({
        "access_token": access_token,
        "refresh_token": refresh_token,
        "user": user,
    }))
    .into_response()
}

#[derive(serde::Deserialize)]
struct OtpRequestBody {
    email: String,
}

/// POST /api/otp/request — passwordless login step 1 (WP12). Enumeration-
/// consistent: always returns 200 with a challenge id (a decoy when the account
/// is unknown or has no verified channel). Delivery is fire-and-forget so the
/// response time never leaks account existence.
async fn handle_otp_request(
    State(state): State<Arc<AppState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(body): Json<OtpRequestBody>,
) -> impl IntoResponse {
    let ip = addr.ip();
    if !check_login_rate_limit(ip, &body.email) {
        return (
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            Json(serde_json::json!({"error": "too many attempts, try again later"})),
        )
            .into_response();
    }

    match state.user_db.request_otp(&body.email) {
        Ok(Some(challenge)) => {
            let cid = challenge.challenge_id.clone();
            let deliverer = state.otp_delivery.clone();
            let user_db = state.user_db.clone();
            let (user_id, channel, chat_id, code) = (
                challenge.user_id.clone(),
                challenge.channel.clone(),
                challenge.channel_user_id.clone(),
                challenge.code.clone(),
            );
            tokio::spawn(async move {
                let text =
                    format!("🐾 DuDuClaw 登入驗證碼：{code}\n5 分鐘內有效，請勿分享給任何人。");
                match deliverer.deliver(&channel, &chat_id, &text).await {
                    Ok(()) => {
                        let _ = user_db.log_action(
                            Some(&user_id),
                            "otp_sent",
                            Some(&channel),
                            None,
                            None,
                        );
                    }
                    Err(e) => {
                        warn!("OTP delivery failed: {e}");
                        let _ = user_db.log_action(
                            Some(&user_id),
                            "otp_delivery_failed",
                            Some(&channel),
                            Some(&e),
                            None,
                        );
                    }
                }
            });
            // Uniform response shape — no `hint` field, so a real account is
            // indistinguishable from an unknown one (Haiku review #1: the mere
            // presence of `hint` was an enumeration oracle). The FE shows a
            // generic "if the account has a linked channel, a code was sent".
            Json(serde_json::json!({ "challenge_id": cid, "sent": true })).into_response()
        }
        Ok(None) => Json(serde_json::json!({
            "challenge_id": uuid::Uuid::new_v4().to_string(),
            "sent": true,
        }))
        .into_response(),
        Err(_) => (
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            Json(serde_json::json!({"error": "too many codes requested, try again shortly"})),
        )
            .into_response(),
    }
}

#[derive(serde::Deserialize)]
struct OtpVerifyBody {
    challenge_id: String,
    code: String,
}

/// POST /api/otp/verify — passwordless login step 2 (WP12). On success issues
/// the same JWT pair as password login; every failure collapses to one generic
/// 401 (no oracle for code-guessing).
async fn handle_otp_verify(
    State(state): State<Arc<AppState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(body): Json<OtpVerifyBody>,
) -> impl IntoResponse {
    let ip = addr.ip();
    // Per-IP throttle on verification (Haiku review #2) — bounds distributed
    // code-guessing beyond the per-challenge attempt cap.
    if !check_otp_verify_rate_limit(ip) {
        return (
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            Json(serde_json::json!({"error": "too many attempts, try again later"})),
        )
            .into_response();
    }
    let user = match state.user_db.verify_otp(&body.challenge_id, &body.code) {
        Ok(u) => u,
        Err(_) => {
            let ip_str = ip.to_string();
            let _ = state
                .user_db
                .log_action(None, "otp_login_failed", None, None, Some(&ip_str));
            return (
                axum::http::StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"error": "invalid or expired code"})),
            )
                .into_response();
        }
    };

    let bindings = state.user_db.get_user_agents(&user.id).unwrap_or_default();
    let agent_access: Vec<(String, duduclaw_auth::AccessLevel)> = bindings
        .iter()
        .map(|b| (b.agent_name.clone(), b.access_level))
        .collect();

    let access_token = match state.jwt_config.issue_access_token(&user, &agent_access) {
        Ok(t) => t,
        Err(e) => {
            error!("Failed to issue access token: {e}");
            return (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "token generation failed"})),
            )
                .into_response();
        }
    };
    let refresh_token = match state.jwt_config.issue_refresh_token(&user.id) {
        Ok(t) => t,
        Err(e) => {
            error!("Failed to issue refresh token: {e}");
            return (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "token generation failed"})),
            )
                .into_response();
        }
    };

    let ip_str = ip.to_string();
    let _ = state
        .user_db
        .log_action(Some(&user.id), "login_otp", None, None, Some(&ip_str));

    Json(serde_json::json!({
        "access_token": access_token,
        "refresh_token": refresh_token,
        "user": user,
    }))
    .into_response()
}

#[derive(serde::Deserialize)]
struct ChannelBindBody {
    user_id: String,
    channel: String,
    channel_user_id: String,
}

/// POST /api/channel-identity/bind — admin-only (WP12 T12.3, admin-prefill path):
/// bind and verify a user's 1:1 channel DM identity so they can log in via OTP.
/// Fail-closed: the authoritative role is re-read from the DB, not trusted from
/// the token. Self-service verified binding via a DM handshake is a follow-up.
async fn handle_channel_bind(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Json(body): Json<ChannelBindBody>,
) -> impl IntoResponse {
    // Fail-closed input validation (Haiku review #4).
    const OTP_CHANNELS: [&str; 4] = ["telegram", "line", "discord", "slack"];
    if body.user_id.is_empty()
        || body.user_id.len() > 255
        || body.channel_user_id.is_empty()
        || body.channel_user_id.len() > 512
        || !OTP_CHANNELS.contains(&body.channel.as_str())
    {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "invalid channel binding request"})),
        )
            .into_response();
    }
    let token = match extract_bearer_token(&headers) {
        Some(t) => t,
        None => {
            return (
                axum::http::StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"error": "missing Authorization header"})),
            )
                .into_response();
        }
    };
    let claims = match state.jwt_config.verify_access_token(token) {
        Ok(c) => c,
        _ => {
            return (
                axum::http::StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"error": "invalid or expired token"})),
            )
                .into_response();
        }
    };
    let caller = match state.user_db.get_user(&claims.sub) {
        Ok(Some(u)) => u,
        _ => {
            return (
                axum::http::StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"error": "user not found"})),
            )
                .into_response();
        }
    };
    if caller.role != duduclaw_auth::UserRole::Admin {
        return (
            axum::http::StatusCode::FORBIDDEN,
            Json(serde_json::json!({"error": "admin required"})),
        )
            .into_response();
    }
    // Never bind an orphan identity to a non-existent user (fail-closed).
    if !matches!(state.user_db.get_user(&body.user_id), Ok(Some(_))) {
        return (
            axum::http::StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "target user not found"})),
        )
            .into_response();
    }
    match state.user_db.bind_channel_identity(
        &body.user_id,
        &body.channel,
        &body.channel_user_id,
        true,
    ) {
        Ok(()) => {
            let _ = state.user_db.log_action(
                Some(&caller.id),
                "channel_identity_bound",
                Some(&body.user_id),
                Some(&body.channel),
                None,
            );
            Json(serde_json::json!({"success": true})).into_response()
        }
        Err(e) => (
            axum::http::StatusCode::CONFLICT,
            Json(serde_json::json!({"error": e})),
        )
            .into_response(),
    }
}

#[derive(serde::Deserialize)]
struct ChannelIdentityListQuery {
    user_id: String,
}

/// GET /api/channel-identity/list?user_id=<id> — admin-only, read-only
/// listing of a user's verified channel DM identities (WP-B, 2026-08-12 IA
/// audit §2-1: this data drives `approver_links`/channel-side approvals but
/// previously had zero dashboard surface — see `decision_notify.rs`).
/// Same auth posture as `handle_channel_bind`: admin JWT required, target
/// user existence checked, fail-closed on every branch.
async fn handle_channel_identity_list(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Query(q): Query<ChannelIdentityListQuery>,
) -> impl IntoResponse {
    if q.user_id.is_empty() || q.user_id.len() > 255 {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "invalid user_id"})),
        )
            .into_response();
    }
    let token = match extract_bearer_token(&headers) {
        Some(t) => t,
        None => {
            return (
                axum::http::StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"error": "missing Authorization header"})),
            )
                .into_response();
        }
    };
    let claims = match state.jwt_config.verify_access_token(token) {
        Ok(c) => c,
        _ => {
            return (
                axum::http::StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"error": "invalid or expired token"})),
            )
                .into_response();
        }
    };
    let caller = match state.user_db.get_user(&claims.sub) {
        Ok(Some(u)) => u,
        _ => {
            return (
                axum::http::StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"error": "user not found"})),
            )
                .into_response();
        }
    };
    if caller.role != duduclaw_auth::UserRole::Admin {
        return (
            axum::http::StatusCode::FORBIDDEN,
            Json(serde_json::json!({"error": "admin required"})),
        )
            .into_response();
    }
    if !matches!(state.user_db.get_user(&q.user_id), Ok(Some(_))) {
        return (
            axum::http::StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "target user not found"})),
        )
            .into_response();
    }
    match state.user_db.verified_channels_for_user(&q.user_id) {
        Ok(identities) => Json(serde_json::json!({ "identities": identities })).into_response(),
        Err(e) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e})),
        )
            .into_response(),
    }
}

/// Iterate every agent's wiki under `agents_dir` and run the Phase 3
/// janitor (auto-correct, archive, snapshot sync). Best-effort — failures
/// are logged and the loop continues.
fn run_wiki_janitor_pass(
    agents_dir: &std::path::Path,
    store: &Arc<duduclaw_memory::WikiTrustStore>,
    janitor_cfg: &duduclaw_memory::JanitorConfig,
) {
    let entries = match std::fs::read_dir(agents_dir) {
        Ok(e) => e,
        Err(e) => {
            warn!(path = %agents_dir.display(), error = %e, "wiki janitor: agents dir unreadable");
            return;
        }
    };
    let janitor = duduclaw_memory::WikiJanitor::with_config(store.clone(), *janitor_cfg);

    // (review HIGH-DB N3) Run global retention pruning ONCE per cycle, not
    // per agent. Doing it per agent meant the pruning budget was multiplied
    // by agent count, and rate / conv_cap deletes did the same work N times.
    match janitor.run_global_retention() {
        Ok((h, r, c)) => info!(
            history_pruned = h,
            rate_pruned = r,
            conv_cap_pruned = c,
            "wiki trust retention pruned"
        ),
        Err(e) => warn!(error = %e, "wiki trust retention pruning failed"),
    }

    for entry in entries.flatten() {
        let agent_dir = entry.path();
        if !agent_dir.is_dir() {
            continue;
        }
        let agent_id = match agent_dir.file_name().and_then(|n| n.to_str()) {
            Some(name) => name.to_string(),
            None => continue,
        };
        let wiki_dir = agent_dir.join("wiki");
        if !wiki_dir.exists() {
            continue;
        }
        let report = janitor.run_once(&wiki_dir, &agent_id);
        if !report.corrected_pages.is_empty()
            || !report.archived_pages.is_empty()
            || report.snapshot_synced > 0
        {
            info!(
                agent = %agent_id,
                corrected = report.corrected_pages.len(),
                archived = report.archived_pages.len(),
                snapshots = report.snapshot_synced,
                "wiki janitor pass produced changes"
            );
        }
    }
}

/// Refresh endpoint rate limiter window and budget.
///
/// H9 originally set this to 10/5min, but that is far too tight for a real
/// session: each page (re)load runs `loadFromStorage` (up to 4 retries on a
/// transient failure) and every open tab plus the 25-min auto-refresh timer
/// all hit `/api/refresh`. A user navigating and reloading a few times inside
/// the window exhausted 10 quickly, the client's retries burned the rest, and
/// `loadFromStorage` fell through to the login screen (Bug#2). 60/5min keeps a
/// meaningful abuse ceiling (this endpoint only exchanges a valid refresh
/// token) while leaving ample headroom for legitimate multi-tab use.
const REFRESH_RATE_WINDOW_SECS: u64 = 300;
const REFRESH_RATE_MAX: u32 = 60;

static REFRESH_RATE_LIMITER: std::sync::LazyLock<Mutex<HashMap<IpAddr, (Instant, u32)>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

/// Returns `Ok(())` when within budget, or `Err(retry_after_secs)` when the IP
/// is over the limit (so the caller can emit a `Retry-After` header).
fn check_refresh_rate_limit(ip: IpAddr) -> Result<(), u64> {
    let mut map = REFRESH_RATE_LIMITER
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let now = Instant::now();
    if map.len() > 10000 {
        map.retain(|_, (t, _)| now.duration_since(*t).as_secs() < REFRESH_RATE_WINDOW_SECS);
    }
    let entry = map.entry(ip).or_insert((now, 0));
    let elapsed = now.duration_since(entry.0).as_secs();
    if elapsed > REFRESH_RATE_WINDOW_SECS {
        *entry = (now, 1);
        return Ok(());
    }
    entry.1 += 1;
    if entry.1 <= REFRESH_RATE_MAX {
        Ok(())
    } else {
        Err(REFRESH_RATE_WINDOW_SECS.saturating_sub(elapsed).max(1))
    }
}

/// POST /api/refresh — Exchange a refresh token for a new access token.
async fn handle_refresh(
    State(state): State<Arc<AppState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(body): Json<RefreshRequest>,
) -> impl IntoResponse {
    // H9 fix: rate limit refresh endpoint (60/5min — see REFRESH_RATE_MAX).
    if let Err(retry_after) = check_refresh_rate_limit(addr.ip()) {
        return (
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            [(axum::http::header::RETRY_AFTER, retry_after.to_string())],
            Json(serde_json::json!({"error": "too many refresh attempts"})),
        )
            .into_response();
    }

    // Verify refresh token — generic error messages to prevent info leakage
    let claims = match state.jwt_config.verify_refresh_token(&body.refresh_token) {
        Ok(c) => c,
        Err(_) => {
            return (
                axum::http::StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"error": "invalid or expired refresh token"})),
            )
                .into_response();
        }
    };

    // Fetch fresh user data and bindings
    let user = match state.user_db.get_user(&claims.sub) {
        Ok(Some(u)) if u.status == duduclaw_auth::UserStatus::Active => u,
        _ => {
            return (
                axum::http::StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"error": "user not found or inactive"})),
            )
                .into_response();
        }
    };

    let bindings = state.user_db.get_user_agents(&user.id).unwrap_or_default();
    let agent_access: Vec<(String, duduclaw_auth::AccessLevel)> = bindings
        .iter()
        .map(|b| (b.agent_name.clone(), b.access_level))
        .collect();

    let access_token = match state.jwt_config.issue_access_token(&user, &agent_access) {
        Ok(t) => t,
        Err(e) => {
            error!("Failed to issue access token: {e}");
            return (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "token generation failed"})),
            )
                .into_response();
        }
    };

    Json(serde_json::json!({"access_token": access_token})).into_response()
}

/// GET /api/me — Return the current user's info from the Authorization header.
async fn handle_me(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
) -> impl IntoResponse {
    let token = match extract_bearer_token(&headers) {
        Some(t) => t,
        None => {
            return (
                axum::http::StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"error": "missing Authorization header"})),
            )
                .into_response();
        }
    };

    let claims = match state.jwt_config.verify_access_token(token) {
        Ok(c) => c,
        _ => {
            return (
                axum::http::StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"error": "invalid or expired token"})),
            )
                .into_response();
        }
    };

    let user = match state.user_db.get_user(&claims.sub) {
        Ok(Some(u)) => u,
        _ => {
            return (
                axum::http::StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": "user not found"})),
            )
                .into_response();
        }
    };

    let bindings = state.user_db.get_user_agents(&user.id).unwrap_or_default();

    Json(serde_json::json!({
        "user": user,
        "bindings": bindings,
    }))
    .into_response()
}

#[derive(serde::Deserialize)]
struct ChangePasswordRequest {
    new_password: String,
}

/// POST /api/change-password — Set a new password for the authenticated user.
///
/// Intentionally does NOT pass through `authenticate_jwt`, so a user flagged
/// `must_change_password` (e.g. the bootstrap admin) can recover. A valid access
/// token (issued at login) is required; possession of it proves the caller knew
/// the current password. Clears the forced-change flag on success.
async fn handle_change_password(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Json(body): Json<ChangePasswordRequest>,
) -> impl IntoResponse {
    let token = match extract_bearer_token(&headers) {
        Some(t) => t,
        None => {
            return (
                axum::http::StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"error": "missing Authorization header"})),
            )
                .into_response();
        }
    };

    let claims = match state.jwt_config.verify_access_token(token) {
        Ok(c) => c,
        _ => {
            return (
                axum::http::StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"error": "invalid or expired token"})),
            )
                .into_response();
        }
    };

    if body.new_password.chars().count() < 8 {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "password must be at least 8 characters"})),
        )
            .into_response();
    }

    match state
        .user_db
        .update_user(&claims.sub, None, None, Some(&body.new_password))
    {
        Ok(()) => {
            let _ =
                state
                    .user_db
                    .log_action(Some(&claims.sub), "change_password", None, None, None);
            Json(serde_json::json!({"ok": true})).into_response()
        }
        Err(e) => {
            warn!(user = %claims.sub, "change-password failed: {e}");
            (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "failed to update password"})),
            )
                .into_response()
        }
    }
}

#[derive(serde::Deserialize)]
struct FirstRunClaimRequest {
    password: String,
}

/// GET /api/first-run/status — report whether this instance is unclaimed, so the
/// LoginPage can show a "set your admin password" form instead of demanding the
/// console one-time password (the onboarding chicken-and-egg).
///
/// Loopback-only: off-loopback callers always see `claimable: false` so the
/// unclaimed state is never advertised to the network.
async fn handle_first_run_status(
    State(state): State<Arc<AppState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
) -> impl IntoResponse {
    let claimable = addr.ip().is_loopback() && state.user_db.is_unclaimed_default_admin();
    Json(serde_json::json!({ "claimable": claimable }))
}

/// POST /api/first-run/claim — set the initial `admin@local` password WITHOUT an
/// old password, so a first-time operator (incl. Desktop-app users with no
/// console) can get in. Fail-closed on three gates:
///   1. loopback caller only (a remote attacker cannot reach the flow);
///   2. instance still unclaimed (`must_change_password = 1`) — enforced
///      atomically inside `claim_default_admin`, so it is single-shot;
///   3. minimum password length.
/// After a successful claim the flag is cleared and the endpoint goes inert.
async fn handle_first_run_claim(
    State(state): State<Arc<AppState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(body): Json<FirstRunClaimRequest>,
) -> impl IntoResponse {
    if !addr.ip().is_loopback() {
        return (
            axum::http::StatusCode::FORBIDDEN,
            Json(serde_json::json!({"error": "first-run setup is only available from localhost"})),
        )
            .into_response();
    }
    if body.password.chars().count() < 8 {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "password must be at least 8 characters"})),
        )
            .into_response();
    }
    match state.user_db.claim_default_admin(&body.password) {
        Ok(true) => {
            let _ = state
                .user_db
                .log_action(None, "first_run_claim", None, None, None);
            Json(serde_json::json!({"ok": true})).into_response()
        }
        Ok(false) => (
            axum::http::StatusCode::CONFLICT,
            Json(serde_json::json!({"error": "this instance has already been set up"})),
        )
            .into_response(),
        Err(e) => {
            warn!("first-run claim failed: {e}");
            (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "failed to set password"})),
            )
                .into_response()
        }
    }
}

// ── D4a: OOBE pre-auth network setup ─────────────────────────────────────
//
// The OOBE flow's order is "network step, THEN account step" (design
// `DESIGN-network-settings-2026-08.md` §5.1) — the network step runs before
// any account exists, so `require_admin!()`'s WS-RPC gate can never be
// satisfied yet. These three routes are the pre-auth twin of `network.*`,
// shaped exactly like the existing `/api/first-run/claim` flow above:
// loopback-only + unclaimed-instance-only, with one extra condition
// `/api/first-run/claim` doesn't need — appliance-only, since this whole
// feature is meaningless off the appliance image (a laptop dev build has no
// iwd to drive).

/// Fail-closed gate shared by all three `/api/first-run/network/*` routes:
/// loopback caller, instance still unclaimed, AND running on the appliance
/// image. Every failure returns the exact SAME message regardless of which
/// condition tripped — matching `handle_local_session`'s "an off-loopback
/// prober must not be able to learn the edition, the switch state, or which
/// condition it tripped" discipline (and `handle_first_run_status`'s
/// analogous loopback-only rule) — a probe from off-loopback, or one that
/// arrives after claim, or one against a non-appliance build, all look
/// identical from the outside.
fn first_run_network_gate(state: &AppState, addr: SocketAddr) -> Option<axum::response::Response> {
    let allowed = addr.ip().is_loopback()
        && state.user_db.is_unclaimed_default_admin()
        && duduclaw_core::is_appliance();
    if allowed {
        return None;
    }
    // 2026-09-05: say WHICH gate refused. The desktop shell re-running OOBE
    // after the admin account exists hits this from loopback on an
    // appliance, and needs to tell "setup already done" (network was
    // configured once; carry on) apart from a genuinely wrong caller.
    let code = if !duduclaw_core::is_appliance() {
        "not_appliance"
    } else if !addr.ip().is_loopback() {
        "not_loopback"
    } else {
        "first_run_completed"
    };
    Some(
        (
            axum::http::StatusCode::FORBIDDEN,
            Json(serde_json::json!({
                "error": "first-run network setup is only available from localhost on an appliance before setup",
                "code": code,
            })),
        )
            .into_response(),
    )
}

/// `{"ok": false, "code": ..., "message": ...}` at HTTP 200 — design §5.1's
/// deliberate choice of an envelope over HTTP status semantics: the shell is
/// a hand-rolled HTTP/1.1 client (see `duduclaw-shell/src/oobe/claim.rs`),
/// and a single explicit `code` field it can switch on is far more robust
/// than asking it to correctly interpret 4xx/5xx nuance. Body-parse failures
/// and the three gate conditions above are NOT rendered this way — those
/// are 400/403 respectively, because they are not one of the closed nine
/// [`crate::network::WifiErrorCode`] outcomes this envelope exists for.
fn network_error_envelope(err: &crate::network::WifiError) -> axum::response::Response {
    Json(serde_json::json!({
        "ok": false,
        "code": err.code.code(),
        "message": err.code.message(),
    }))
    .into_response()
}

/// GET /api/first-run/network/status
async fn handle_first_run_network_status(
    State(state): State<Arc<AppState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
) -> axum::response::Response {
    if let Some(denied) = first_run_network_gate(&state, addr) {
        return denied;
    }
    // `network::status()` never returns `Err` in the current implementation
    // (see that function's own doc) — the `Err` arm below is symmetry with
    // the other two handlers, not reachable dead code by design.
    match crate::network::status().await {
        Ok(status) => match serde_json::to_value(&status) {
            Ok(v) => Json(serde_json::json!({"ok": true, "result": v})).into_response(),
            Err(e) => {
                warn!("first-run network status serialize failed: {e}");
                network_error_envelope(&crate::network::WifiError {
                    code: crate::network::WifiErrorCode::BackendUnavailable,
                    detail: e.to_string(),
                })
            }
        },
        Err(err) => network_error_envelope(&err),
    }
}

#[derive(serde::Deserialize)]
struct FirstRunNetworkScanRequest {
    /// `None` (field omitted) defaults to `true` — a fresh scan — matching
    /// `network.wifi_scan`'s own default.
    #[serde(default)]
    rescan: Option<bool>,
}

/// POST /api/first-run/network/scan
async fn handle_first_run_network_scan(
    State(state): State<Arc<AppState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(body): Json<FirstRunNetworkScanRequest>,
) -> axum::response::Response {
    if let Some(denied) = first_run_network_gate(&state, addr) {
        return denied;
    }
    let rescan = body.rescan.unwrap_or(true);
    match crate::network::wifi_scan(rescan).await {
        Ok(result) => Json(
            serde_json::json!({"ok": true, "result": crate::network::scan_result_to_json(&result)}),
        )
        .into_response(),
        Err(err) => network_error_envelope(&err),
    }
}

/// POST /api/first-run/network/connect
///
/// Deliberately has NO `/api/first-run/network/forget` twin — OOBE has no
/// "forget this network" UI at all (there is nothing yet to forget on a
/// freshly-provisioned appliance), so a pre-auth forget endpoint would only
/// be extra pre-auth attack surface for zero product value — minimal attack
/// surface wins (design §5.1).
async fn handle_first_run_network_connect(
    State(state): State<Arc<AppState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(body): Json<crate::network::WifiConnectRequest>,
) -> axum::response::Response {
    if let Some(denied) = first_run_network_gate(&state, addr) {
        return denied;
    }
    if body.ssid.trim().is_empty() {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "ssid must not be empty"})),
        )
            .into_response();
    }
    let result = crate::network::wifi_connect(&body.ssid, body.psk.as_deref()).await;

    // Audited exactly like the `network.wifi_connect` RPC (design §3.2), and
    // arguably MORE important here: this path has no authenticated caller to
    // attribute — that is the whole point of a pre-auth route — so the audit
    // row is the only record that the box's network was changed at all, and
    // `source` distinguishes it from a dashboard-initiated change. Same
    // payload discipline as the RPC: SSID, outcome, error class. Never the
    // passphrase, and never a "was one supplied" flag either (that alone is
    // password-shaped metadata).
    let (ok, code) = match &result {
        Ok(()) => (true, None),
        Err(e) => (false, Some(e.code.code())),
    };
    crate::security_autopilot::audit_and_emit(
        &state.home_dir,
        &duduclaw_security::audit::AuditEvent::new(
            "wifi_connect",
            &body.ssid,
            duduclaw_security::audit::Severity::Info,
            serde_json::json!({ "ssid": body.ssid, "ok": ok, "code": code, "source": "first_run_oobe" }),
        ),
    );

    match result {
        Ok(()) => Json(
            serde_json::json!({"ok": true, "result": {"state": "connected", "ssid": body.ssid}}),
        )
        .into_response(),
        Err(err) => Json(serde_json::json!({
            "ok": false,
            "code": err.code.code(),
            "message": err.code.message_with_ssid(&body.ssid),
        }))
        .into_response(),
    }
}

/// POST /api/session/local — Personal-edition passwordless local session
/// (WP-F1, design §2.3, decision D3 = plan A).
///
/// Issues a **normal** JWT pair for the real `admin@local` user to a caller
/// that has proven it is sitting at this machine. Nothing downstream changes:
/// the token is indistinguishable from one obtained via `/api/login`, so every
/// `require_admin!` site, the WS handshake, and audit attribution keep working
/// unmodified. The only new thing is how the token is obtained.
///
/// The six-condition gate lives in [`crate::local_session::evaluate`] (pure,
/// unit-tested per branch). Every failure returns the SAME 403 body: an
/// off-loopback prober must not be able to learn the edition, the switch
/// state, or which condition it tripped — same rule as
/// `handle_first_run_status` never advertising `claimable` off-loopback.
///
/// One subtlety worth stating: the bootstrap `admin@local` carries
/// `must_change_password = 1`, and `authenticate_jwt` refuses *all* operations
/// while that flag is set — so a token issued without clearing it would be
/// dead on its very next request. The endpoint therefore performs an implicit
/// claim with a random password it immediately discards (§2.3). "No password
/// prompt" never becomes "empty password"; the operator can still set one of
/// their own later from account settings before exposing the port.
async fn handle_local_session(
    State(state): State<Arc<AppState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: axum::http::HeaderMap,
) -> impl IntoResponse {
    use crate::local_session;

    /// One uniform refusal — never says which condition failed.
    fn refused() -> axum::response::Response {
        (
            axum::http::StatusCode::FORBIDDEN,
            Json(serde_json::json!({"error": "local auto-login unavailable"})),
        )
            .into_response()
    }

    let enabled = local_session::auto_login_enabled(&state.home_dir);
    let is_personal = state.handler.resolve_edition_profile().await.is_personal();
    let origin_allowed = origin_is_allowed(&headers);

    if let Err(denial) = local_session::evaluate(
        enabled,
        is_personal,
        addr.ip().is_loopback(),
        origin_allowed,
        &headers,
    ) {
        // Local-only diagnostic; the client learns nothing from it.
        tracing::debug!(reason = denial.as_str(), "local session refused");
        return refused();
    }

    // Implicit claim — single-shot and atomic inside the DB (a racing claim
    // affects zero rows). `Ok(false)` just means someone else claimed first,
    // which is exactly as good for us.
    if state.user_db.is_unclaimed_default_admin() {
        if let Err(e) = state.user_db.claim_default_admin_random() {
            error!("local session: implicit claim failed: {e}");
            return (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "failed to establish local session"})),
            )
                .into_response();
        }
    }

    let user = match state
        .user_db
        .get_user_by_email(local_session::LOCAL_ADMIN_EMAIL)
    {
        Ok(Some(u)) => u,
        // No bootstrap admin (operator deleted/renamed it) — fall back to the
        // login page rather than inventing an identity.
        Ok(None) => return refused(),
        Err(e) => {
            error!("local session: admin lookup failed: {e}");
            return refused();
        }
    };

    // Fail closed on the same two conditions `authenticate_jwt` enforces, so we
    // never hand out a token that would be rejected on its next use.
    if user.status != duduclaw_auth::UserStatus::Active || user.must_change_password {
        return refused();
    }

    let bindings = state.user_db.get_user_agents(&user.id).unwrap_or_default();
    let agent_access: Vec<(String, duduclaw_auth::AccessLevel)> = bindings
        .iter()
        .map(|b| (b.agent_name.clone(), b.access_level))
        .collect();

    let access_token = match state.jwt_config.issue_access_token(&user, &agent_access) {
        Ok(t) => t,
        Err(e) => {
            error!("local session: failed to issue access token: {e}");
            return (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "token generation failed"})),
            )
                .into_response();
        }
    };
    let refresh_token = match state.jwt_config.issue_refresh_token(&user.id) {
        Ok(t) => t,
        Err(e) => {
            error!("local session: failed to issue refresh token: {e}");
            return (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "token generation failed"})),
            )
                .into_response();
        }
    };

    let _ = state.user_db.update_last_login(&user.id);
    // Attributed to the real user id — the reason plan A was chosen over
    // synthesising an `admin_fallback` context whose audit rows read "system".
    let ip_str = addr.ip().to_string();
    let _ = state.user_db.log_action(
        Some(&user.id),
        "login_local_auto",
        None,
        None,
        Some(&ip_str),
    );

    Json(serde_json::json!({
        "access_token": access_token,
        "refresh_token": refresh_token,
        "user": user,
    }))
    .into_response()
}

/// Built-in loopback origins that are always allowed for the local dashboard,
/// independent of any operator configuration.
const BUILTIN_ALLOWED_ORIGINS: &[&str] = &["localhost", "127.0.0.1", "[::1]"];

/// Operator-configured *extra* allowed origins (config.toml
/// `[gateway] allowed_origins` merged with the `DUDUCLAW_ALLOWED_ORIGINS` env).
/// Stored normalized to the `host[:port]` form `origin_host_matches` expects.
/// Empty (the default) => behaviour is byte-identical to loopback-only.
///
/// Wrapped in an `RwLock` so the dashboard (`system.update_config`) can hot-apply
/// a new allowlist without a gateway restart: `origin_is_allowed` takes a read
/// lock per request, [`set_allowed_origins`] takes the write lock. The read cost
/// is a single uncontended lock acquisition on the WS-upgrade path.
static ALLOWED_ORIGINS: std::sync::OnceLock<std::sync::RwLock<Vec<String>>> =
    std::sync::OnceLock::new();

/// Lazily-initialized backing cell for [`ALLOWED_ORIGINS`]. Starts empty
/// (loopback-only) until [`init_allowed_origins`] runs at startup.
fn allowed_origins_cell() -> &'static std::sync::RwLock<Vec<String>> {
    ALLOWED_ORIGINS.get_or_init(|| std::sync::RwLock::new(Vec::new()))
}

/// Read + normalize the `DUDUCLAW_ALLOWED_ORIGINS` env entries (comma-separated).
/// Re-read on every hot-update so a dashboard save never drops env-provided
/// origins (the UI only ever knows about the config.toml portion).
fn env_allowed_origins() -> Vec<String> {
    std::env::var("DUDUCLAW_ALLOWED_ORIGINS")
        .ok()
        .map(|v| v.split(',').filter_map(normalize_origin_entry).collect())
        .unwrap_or_default()
}

/// Normalize a user-supplied origin allowlist entry into the `host[:port]`
/// form `origin_host_matches` expects: trim, strip a leading scheme
/// (`http://` / `https://` / `ws://` / `wss://`, case-insensitive), strip a
/// trailing `/`. Returns `None` for entries that are empty after cleaning.
/// No wildcard support — each entry is an exact host or host:port.
pub(crate) fn normalize_origin_entry(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let lower = trimmed.to_ascii_lowercase();
    let mut start = 0;
    for scheme in ["http://", "https://", "ws://", "wss://"] {
        if lower.starts_with(scheme) {
            start = scheme.len();
            break;
        }
    }
    let cleaned = trimmed[start..].trim_end_matches('/').trim();
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned.to_string())
    }
}

/// Install the operator-configured extra allowed origins once at startup.
/// `raw` is the already-merged config.toml + env list from the CLI. Raw entries
/// are normalized (see [`normalize_origin_entry`]) and empties dropped. Returns
/// the normalized list so the caller can log it.
pub(crate) fn init_allowed_origins(raw: Vec<String>) -> Vec<String> {
    let normalized: Vec<String> = raw
        .iter()
        .filter_map(|s| normalize_origin_entry(s))
        .collect();
    *allowed_origins_cell().write().unwrap() = normalized.clone();
    normalized
}

/// Hot-apply a new operator allowlist from the given config.toml `[gateway]
/// allowed_origins` entries — used by `system.update_config` so a dashboard save
/// takes effect immediately (no restart). The `DUDUCLAW_ALLOWED_ORIGINS` env
/// entries are re-merged so a UI save never drops env-provided origins. Entries
/// are normalized, empties dropped, deduped (config first, then env). Returns the
/// resulting live list.
pub(crate) fn set_allowed_origins(config_entries: Vec<String>) -> Vec<String> {
    let mut merged: Vec<String> = config_entries
        .iter()
        .filter_map(|s| normalize_origin_entry(s))
        .collect();
    for e in env_allowed_origins() {
        if !merged.contains(&e) {
            merged.push(e);
        }
    }
    *allowed_origins_cell().write().unwrap() = merged.clone();
    merged
}

/// Whether the request's `Origin` is an allowed dashboard origin.
///
/// HS3/C5: uses exact authority matching (any port on the built-in loopback
/// hosts + any operator-configured `allowed_origins`). Absent Origin
/// (non-browser clients like curl/SDK) is allowed. Rejects suffix-attack
/// origins such as `http://localhost.evil.com`.
pub(crate) fn origin_is_allowed(headers: &axum::http::HeaderMap) -> bool {
    let guard = allowed_origins_cell().read().unwrap();
    origin_is_allowed_with(headers, guard.as_slice())
}

/// Testable core of [`origin_is_allowed`]: matches against the built-in
/// loopback origins plus the given `extra` list (already normalized to
/// `host[:port]`), without touching the process-wide `OnceLock`.
pub(crate) fn origin_is_allowed_with(headers: &axum::http::HeaderMap, extra: &[String]) -> bool {
    match headers.get("origin").and_then(|v| v.to_str().ok()) {
        None => true,
        Some(origin) => {
            let mut allowed: Vec<&str> = BUILTIN_ALLOWED_ORIGINS.to_vec();
            allowed.extend(extra.iter().map(String::as_str));
            duduclaw_core::origin_host_matches(origin, &allowed)
        }
    }
}

/// Extract Bearer token from Authorization header.
fn extract_bearer_token(headers: &axum::http::HeaderMap) -> Option<&str> {
    headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
}

// ── Dashboard file panel (WP1.4, I-4 search extension) ───────────
//
// Two Bearer-JWT-gated endpoints let the dashboard list and download the
// documents an AI staff member produced/received under its attachments dir:
//   GET /api/files?agent=<id>                     → JSON {"files": [...]}
//   GET /api/files/download?agent=<id>&name=<f>   → streamed file
// When `agent` is omitted both fall back to the shared `<home>/attachments/`.
// Path safety lives in `crate::files_api` (allowlist + canonicalize
// containment, fail-closed); see its unit tests.
//
// I-4 ("產物與檔案"): `GET /api/files` additionally accepts, all optional
// and AND-combined, applied AFTER provenance is attached so `q` can match
// the ledger's display name / origin too:
//   q=<text>        search: archived name / display name / origin
//   task_id=<id>    filter to files the I-2b ledger ties to this task
//   since=<ms>      inclusive lower bound on mtime, Unix epoch ms
//   until=<ms>      inclusive upper bound on mtime, Unix epoch ms
// The response shape is unchanged (`{"files": [...]}`) — these only narrow
// which rows are included, never add new top-level fields.

/// Refuse a REST caller whose account still carries `must_change_password`.
///
/// `authenticate_jwt` used to refuse such a caller outright (see its own doc
/// comment / `jwt_account_gate`), which fail-closed every REST route right
/// along with the WS handshake. Now that it authenticates a flagged account
/// instead of erroring, each REST helper below calls this immediately after
/// `authenticate_jwt` succeeds so the pre-fix fail-closed behaviour is
/// preserved for every route except the one that is deliberately exempt:
/// `POST /api/change-password` (`handle_change_password`) does not call
/// `authenticate_jwt` at all, precisely so a flagged account can still reach
/// it. This mirrors `handlers.rs::is_password_change_allowlisted` on the WS
/// RPC side and shares its machine-readable error code.
fn require_password_changed(ctx: &UserContext) -> Result<(), axum::response::Response> {
    if ctx.must_change_password {
        return Err((
            axum::http::StatusCode::FORBIDDEN,
            Json(serde_json::json!({
                "error": "password change required before any operation",
                "code": crate::handlers::MUST_CHANGE_PASSWORD_ERROR_CODE,
            })),
        )
            .into_response());
    }
    Ok(())
}

/// Authenticate a file request and authorize it for the requested `agent`,
/// mirroring the per-agent fail-closed gate the dashboard RPC layer applies.
///
/// The JWT is taken from the `Authorization` header OR the `token_query`
/// (browser preview/download links can't set a header). Then:
///   - `Some(agent)` → the user must be able to access that agent
///     (`can_access_agent`; admins pass all).
///   - `None` (shared `<home>/attachments/` bucket) → admin only — the shared
///     bucket belongs to no single agent, so non-admins (who are scoped to
///     their bound agents) are denied.
///
/// Returns an `into_response()`-ready 401/403 on failure.
fn authorize_file_access(
    state: &AppState,
    headers: &axum::http::HeaderMap,
    token_query: Option<&str>,
    agent: Option<&str>,
) -> Result<(), axum::response::Response> {
    let unauthorized = || {
        (
            axum::http::StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({ "error": "invalid or expired token" })),
        )
            .into_response()
    };
    let token = extract_bearer_token(headers)
        .or(token_query)
        .ok_or_else(unauthorized)?;
    let ctx = authenticate_jwt(state, token).map_err(|_| unauthorized())?;
    require_password_changed(&ctx)?;

    let allowed = match agent {
        Some(a) => ctx.can_access_agent(a),
        None => ctx.is_admin(),
    };
    if !allowed {
        return Err((
            axum::http::StatusCode::FORBIDDEN,
            Json(serde_json::json!({ "error": "access denied" })),
        )
            .into_response());
    }
    Ok(())
}

/// This gateway has instance-wide admin roles but no tenant claim in its JWT.
/// Causal curation and Decision Lab are therefore admin-only; their requested
/// tenant and ACL values are exact store selectors, not credentials. A future
/// multi-tenant endpoint must bind them to an authenticated tenant identity
/// before widening access.
fn authorize_causal_admin(
    state: &AppState,
    headers: &axum::http::HeaderMap,
) -> Result<UserContext, axum::response::Response> {
    if !origin_is_allowed(headers) {
        return Err((
            axum::http::StatusCode::FORBIDDEN,
            Json(serde_json::json!({ "error": "origin not allowed" })),
        )
            .into_response());
    }
    let unauthorized = || {
        (
            axum::http::StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({ "error": "invalid or expired token" })),
        )
            .into_response()
    };
    let token = extract_bearer_token(headers).ok_or_else(unauthorized)?;
    let ctx = authenticate_jwt(state, token).map_err(|_| unauthorized())?;
    require_password_changed(&ctx)?;
    if !ctx.is_admin() {
        return Err((
            axum::http::StatusCode::FORBIDDEN,
            Json(serde_json::json!({ "error": "admin role required" })),
        )
            .into_response());
    }
    Ok(ctx)
}

fn causal_store_response(
    result: Result<Result<serde_json::Value, CausalStoreError>, tokio::task::JoinError>,
) -> axum::response::Response {
    match result {
        Ok(Ok(value)) => Json(value).into_response(),
        Ok(Err(error)) => match error {
            CausalStoreError::InvalidInput => (
                axum::http::StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": error.to_string() })),
            )
                .into_response(),
            CausalStoreError::NotFound => (
                axum::http::StatusCode::NOT_FOUND,
                Json(serde_json::json!({ "error": error.to_string() })),
            )
                .into_response(),
            CausalStoreError::Conflict | CausalStoreError::MissingSupport => (
                axum::http::StatusCode::CONFLICT,
                Json(serde_json::json!({ "error": error.to_string() })),
            )
                .into_response(),
            // W2-B: fence contention is transient, not malformed input.
            CausalStoreError::Busy => (
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({ "error": error.to_string() })),
            )
                .into_response(),
            CausalStoreError::Sqlite(_) | CausalStoreError::Io(_) => {
                warn!(error = %error, "causal curation store failed");
                (
                    axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({ "error": "causal curation unavailable" })),
                )
                    .into_response()
            }
        },
        Err(error) => {
            warn!(error = %error, "causal curation task failed");
            (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": "causal curation unavailable" })),
            )
                .into_response()
        }
    }
}

fn decision_store_response(
    result: Result<Result<serde_json::Value, DecisionStoreError>, tokio::task::JoinError>,
) -> axum::response::Response {
    match result {
        Ok(Ok(value)) => Json(value).into_response(),
        Ok(Err(error)) => match error {
            DecisionStoreError::Invalid
            | DecisionStoreError::TooLarge
            | DecisionStoreError::Json(_)
            | DecisionStoreError::Simulation(_)
            | DecisionStoreError::Event(_)
            | DecisionStoreError::Empirical(_)
            | DecisionStoreError::PolicySweep(_)
            | DecisionStoreError::Observation(_)
            | DecisionStoreError::SlaHoldout(_) => (
                axum::http::StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": error.to_string() })),
            )
                .into_response(),
            DecisionStoreError::NotFound
            | DecisionStoreError::Causal(CausalStoreError::NotFound) => (
                axum::http::StatusCode::NOT_FOUND,
                Json(serde_json::json!({ "error": error.to_string() })),
            )
                .into_response(),
            DecisionStoreError::VersionConflict | DecisionStoreError::ReviewDenied => (
                axum::http::StatusCode::CONFLICT,
                Json(serde_json::json!({ "error": error.to_string() })),
            )
                .into_response(),
            DecisionStoreError::Revoked => (
                axum::http::StatusCode::GONE,
                Json(serde_json::json!({ "error": error.to_string() })),
            )
                .into_response(),
            DecisionStoreError::CausalStoreRequired => (
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({ "error": error.to_string() })),
            )
                .into_response(),
            DecisionStoreError::Corrupt
            | DecisionStoreError::Causal(_)
            | DecisionStoreError::Ccr(_)
            | DecisionStoreError::Sqlite(_)
            | DecisionStoreError::Io(_)
            | DecisionStoreError::ReviewBroker(_) => {
                warn!(error = %error, "decision dashboard store failed");
                (
                    axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({ "error": "decision dashboard unavailable" })),
                )
                    .into_response()
            }
        },
        Err(error) => {
            warn!(error = %error, "decision dashboard worker failed");
            (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": "decision dashboard unavailable" })),
            )
                .into_response()
        }
    }
}

fn synthetic_adapter_response(
    result: Result<Result<serde_json::Value, SyntheticAdapterError>, tokio::task::JoinError>,
) -> axum::response::Response {
    decision_store_response(result.map(|result| {
        result.map_err(|error| match error {
            SyntheticAdapterError::Invalid => DecisionStoreError::Invalid,
            SyntheticAdapterError::NotFound => DecisionStoreError::NotFound,
            SyntheticAdapterError::Conflict => DecisionStoreError::VersionConflict,
            SyntheticAdapterError::Decision(error) => error,
            SyntheticAdapterError::Causal(error) => DecisionStoreError::Causal(error),
            SyntheticAdapterError::Json(error) => DecisionStoreError::Json(error),
            SyntheticAdapterError::Sqlite(error) => DecisionStoreError::Sqlite(error),
            SyntheticAdapterError::Lifecycle(error) => match error {
                crate::connector_lifecycle::LifecycleError::Invalid => DecisionStoreError::Invalid,
                crate::connector_lifecycle::LifecycleError::NotFound => {
                    DecisionStoreError::NotFound
                }
                crate::connector_lifecycle::LifecycleError::Conflict => {
                    DecisionStoreError::VersionConflict
                }
                crate::connector_lifecycle::LifecycleError::Stale => DecisionStoreError::Revoked,
                crate::connector_lifecycle::LifecycleError::Causal(error) => {
                    DecisionStoreError::Causal(error)
                }
                crate::connector_lifecycle::LifecycleError::Decision(error) => error,
                crate::connector_lifecycle::LifecycleError::Sqlite(error) => {
                    DecisionStoreError::Sqlite(error)
                }
            },
        })
    }))
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CcrDashboardQuery {
    tenant_id: String,
}

/// Recorded evidence is evaluated in memory and scoped to the selected
/// dashboard tenant. The evaluator is shared with `ccr-compare-replay`.
async fn handle_ccr_replay(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Query(query): Query<CcrDashboardQuery>,
    body: axum::body::Bytes,
) -> axum::response::Response {
    if let Err(response) = authorize_causal_admin(&state, &headers) {
        return response;
    }
    if body.len() > crate::ccr_replay::MAX_REPLAY_INPUT_BYTES {
        return (
            axum::http::StatusCode::PAYLOAD_TOO_LARGE,
            Json(serde_json::json!({"error": "replay evidence exceeds 16 MB"})),
        )
            .into_response();
    }
    match tokio::task::spawn_blocking(move || {
        crate::ccr_replay::evaluate_replay_bytes(&body, Some(&query.tenant_id))
    })
    .await
    {
        Ok(Ok(report)) => Json(report).into_response(),
        Ok(Err(_)) => (
            axum::http::StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "invalid replay evidence or tenant scope"})),
        )
            .into_response(),
        Err(_) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": "CCR replay unavailable"})),
        )
            .into_response(),
    }
}

async fn handle_ccr_dashboard(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Query(query): Query<CcrDashboardQuery>,
) -> axum::response::Response {
    if let Err(response) = authorize_causal_admin(&state, &headers) {
        return response;
    }
    let home = state.home_dir.clone();
    match tokio::task::spawn_blocking(move || {
        crate::ccr_dashboard::CcrDashboardStore::from_home(&home).snapshot(&query.tenant_id)
    })
    .await
    {
        Ok(Ok(snapshot)) => Json(snapshot).into_response(),
        Ok(Err(crate::ccr_dashboard::CcrDashboardError::InvalidScope)) => (
            axum::http::StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "invalid tenant selector" })),
        )
            .into_response(),
        Ok(Err(crate::ccr_dashboard::CcrDashboardError::Unavailable)) => (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({ "error": "CCR dashboard unavailable" })),
        )
            .into_response(),
        Ok(Err(crate::ccr_dashboard::CcrDashboardError::InvalidState)) | Err(_) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": "CCR dashboard unavailable" })),
        )
            .into_response(),
    }
}

#[derive(serde::Deserialize)]
#[cfg_attr(test, derive(serde::Serialize))]
#[serde(deny_unknown_fields)]
struct DecisionOverviewQuery {
    tenant_id: String,
    acl: String,
    limit: Option<usize>,
}

#[derive(serde::Deserialize)]
#[cfg_attr(test, derive(serde::Serialize))]
#[serde(deny_unknown_fields)]
struct DecisionTicketSourcesScrubRequest {
    tenant_id: String,
    acl: String,
}

async fn handle_decision_overview(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    axum::extract::RawQuery(raw_query): axum::extract::RawQuery,
) -> axum::response::Response {
    if let Err(response) = authorize_causal_admin(&state, &headers) {
        return response;
    }
    let query = match decision_overview_query(raw_query.as_deref()) {
        Ok(query) => query,
        Err(response) => return response,
    };
    let db = state.home_dir.join("decisions.db");
    decision_store_response(
        tokio::task::spawn_blocking(move || {
            let store = DecisionStore::new(db);
            let scope = DecisionScope {
                tenant_id: query.tenant_id,
                acl: query.acl,
            };
            Ok(serde_json::to_value(
                store.dashboard_overview(&scope, query.limit.unwrap_or(50))?,
            )?)
        })
        .await,
    )
}

async fn handle_decision_ticket_sources_scrub(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    if let Err(response) = authorize_causal_admin(&state, &headers) {
        return response;
    }
    let request: DecisionTicketSourcesScrubRequest = match parse_decision_model_request(&body) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let db = state.home_dir.join("decisions.db");
    decision_store_response(
        tokio::task::spawn_blocking(move || {
            let store = DecisionStore::new(db);
            let scope = DecisionScope {
                tenant_id: request.tenant_id,
                acl: request.acl,
            };
            let scrubbed = store.scrub_expired_ticket_sources(&scope)?;
            let overview = store.dashboard_overview(&scope, 50)?;
            Ok(serde_json::json!({ "scrubbed": scrubbed, "overview": overview }))
        })
        .await,
    )
}

/// X1 方案 1 — run one task-board → decision-twin period on demand.
///
/// Admin-only (same gate as every other decision route). The body may override
/// this run's queue and horizon; everything else (tenant, ACL, retention,
/// the fixed "+1 agent" alternative) comes from `config.toml [decision]`, so
/// an operator cannot mint a pilot under an arbitrary scope through this
/// endpoint.
#[derive(serde::Deserialize, Default)]
#[cfg_attr(test, derive(serde::Serialize))]
#[serde(deny_unknown_fields, default)]
struct DecisionTaskBoardExportRequest {
    /// Agent id, or `all` for the whole board. Omitted ⇒ the configured queue.
    queue: Option<String>,
    /// Complete UTC days in the window. Omitted ⇒ the configured horizon.
    horizon_days: Option<usize>,
}

async fn handle_decision_task_board_export(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    if let Err(response) = authorize_causal_admin(&state, &headers) {
        return response;
    }
    // An empty body means "use the configured defaults".
    let request: DecisionTaskBoardExportRequest = if body.is_empty() {
        DecisionTaskBoardExportRequest::default()
    } else {
        match serde_json::from_slice(&body) {
            Ok(request) => request,
            Err(_) => {
                return (
                    axum::http::StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({"error":"invalid task board export request"})),
                )
                    .into_response();
            }
        }
    };
    let home = state.home_dir.clone();
    let result = tokio::task::spawn_blocking(move || {
        let mut config =
            crate::decision_task_board_shadow::TaskBoardShadowConfig::from_home(&home);
        if let Some(queue) = request.queue {
            let queue = queue.trim().to_owned();
            if queue.is_empty() || queue.len() > 96 {
                return Err("invalid queue".to_string());
            }
            config.queue = queue;
        }
        if let Some(days) = request.horizon_days {
            if !(1..=366).contains(&days) {
                return Err("horizon_days must be 1..=366".to_string());
            }
            config.horizon_days = days;
        }
        crate::decision_task_board_shadow::run_once(&home, &config)
    })
    .await;
    match result {
        Ok(Ok(run)) => Json(run).into_response(),
        Ok(Err(error)) => (
            axum::http::StatusCode::BAD_REQUEST,
            // The message is built from our own strings and store errors —
            // no ticket text or task title ever reaches it.
            Json(serde_json::json!({"error": duduclaw_core::truncate_bytes(&error, 240)})),
        )
            .into_response(),
        Err(_) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error":"task board export failed"})),
        )
            .into_response(),
    }
}

/// X1 方案 4 — build a decision-twin pilot export from an Odoo helpdesk or
/// project queue. Returns the export JSON; importing it is a separate,
/// deliberate step (`/api/decision/import-pilot` or `decision-import-pilot`).
#[derive(serde::Deserialize)]
#[cfg_attr(test, derive(serde::Serialize))]
#[serde(deny_unknown_fields)]
struct DecisionOdooExportRequest {
    agent_id: String,
    #[serde(default)]
    profile: Option<String>,
    /// `helpdesk.ticket` (EE) or `project.task` (CE).
    model: String,
    /// Team id (helpdesk) or project id (project tasks).
    queue: i64,
    since_utc: String,
    until_utc: String,
    horizon_days: usize,
}

async fn handle_decision_odoo_export(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    if let Err(response) = authorize_causal_admin(&state, &headers) {
        return response;
    }
    let request: DecisionOdooExportRequest = match serde_json::from_slice(&body) {
        Ok(request) => request,
        Err(_) => {
            return (
                axum::http::StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error":"invalid odoo export request"})),
            )
                .into_response();
        }
    };
    match crate::decision_odoo_export::export_for_agent(
        &state.home_dir,
        &request.agent_id,
        request.profile.as_deref(),
        &crate::decision_odoo_export::OdooExportRequest {
            model: request.model,
            queue: request.queue,
            since_utc: request.since_utc,
            until_utc: request.until_utc,
            horizon_days: request.horizon_days,
        },
    )
    .await
    {
        Ok(export) => Json(export).into_response(),
        Err(error) => (
            axum::http::StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": duduclaw_core::truncate_bytes(&error, 240)})),
        )
            .into_response(),
    }
}

#[derive(serde::Deserialize)]
#[cfg_attr(test, derive(serde::Serialize))]
#[serde(deny_unknown_fields)]
struct DecisionCatalogQuery {
    tenant_id: String,
    acl: String,
}

#[derive(serde::Deserialize)]
#[cfg_attr(test, derive(serde::Serialize))]
#[serde(deny_unknown_fields)]
struct DecisionShadowMonitorQuery {
    tenant_id: String,
    acl: String,
    policy_id: String,
}

#[derive(serde::Deserialize)]
#[cfg_attr(test, derive(serde::Serialize))]
#[serde(deny_unknown_fields)]
struct DecisionEngineeringValidationRequest {
    tenant_id: String,
    acl: String,
    snapshot_id: String,
    model_version: String,
}

#[derive(serde::Deserialize)]
#[cfg_attr(test, derive(serde::Serialize))]
#[serde(deny_unknown_fields)]
struct DecisionForecastValidationRequest {
    tenant_id: String,
    acl: String,
    snapshot_id: String,
    model_version: String,
    baseline_scenario_id: String,
    alternative_scenario_id: String,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct DecisionModelCandidateCreateRequest {
    tenant_id: String,
    acl: String,
    candidate_id: String,
    screen_replay_hash: String,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct DecisionModelCandidateLoadRequest {
    tenant_id: String,
    acl: String,
    candidate_id: String,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct DecisionModelCandidateCompareRequest {
    tenant_id: String,
    acl: String,
    candidate_id: String,
    target_snapshot_id: String,
    scenario_id: String,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct DecisionModelCandidateLoadRunRequest {
    tenant_id: String,
    acl: String,
    replay_hash: String,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct DecisionModelCandidateScoreRequest {
    tenant_id: String,
    acl: String,
    comparison_run_hash: String,
    outcome_id: String,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct DecisionModelCandidateLoadScoreRequest {
    tenant_id: String,
    acl: String,
    replay_hash: String,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct DecisionOutcomeFitCreateRequest {
    tenant_id: String,
    acl: String,
    fit_id: String,
    outcome_id: String,
    min_saturated_days: usize,
    training_days: usize,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct DecisionOutcomeFitLoadRequest {
    tenant_id: String,
    acl: String,
    fit_id: String,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct DecisionOutcomeScreenSaveRequest {
    tenant_id: String,
    acl: String,
    fit_id: String,
    min_saturated_days: usize,
    min_holdout_days: usize,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct DecisionOutcomeScreenLoadRequest {
    tenant_id: String,
    acl: String,
    replay_hash: String,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct DecisionOutcomeScreenReviewRequest {
    tenant_id: String,
    acl: String,
    replay_hash: String,
    summary: String,
    ttl_seconds: i64,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct DecisionOutcomeScreenReviewStatusRequest {
    tenant_id: String,
    acl: String,
    replay_hash: String,
    approval_id: String,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct DecisionShadowPolicyCreateRequest {
    tenant_id: String,
    acl: String,
    policy_id: String,
    source_lineage: String,
    queue_id: String,
    effective_from_utc: String,
    effective_until_utc: String,
    issue_deadline_seconds: u32,
    min_training_days: usize,
    min_saturated_days: usize,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct DecisionShadowPolicyIdRequest {
    tenant_id: String,
    acl: String,
    policy_id: String,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct DecisionShadowForecastCreateRequest {
    tenant_id: String,
    acl: String,
    forecast_id: String,
    policy_id: String,
    target_day_utc: String,
    known: KnownDayInputs,
    training_artifact_id: Option<String>,
    training_source_json: Option<String>,
    retention_until_utc: Option<String>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct DecisionShadowForecastIdRequest {
    tenant_id: String,
    acl: String,
    forecast_id: String,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct DecisionShadowScoreCreateRequest {
    tenant_id: String,
    acl: String,
    score_id: String,
    forecast_id: String,
    observation_artifact_id: Option<String>,
    observation_source_json: Option<String>,
    retention_until_utc: Option<String>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct DecisionShadowScoreIdRequest {
    tenant_id: String,
    acl: String,
    score_id: String,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct DecisionShadowScreenCriteriaRequest {
    tenant_id: String,
    acl: String,
    policy_id: String,
    min_complete_days: usize,
    min_fixed_coverage_bps: u16,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct DecisionShadowScreenLoadRequest {
    tenant_id: String,
    acl: String,
    replay_hash: String,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct DecisionShadowScreenReviewRequest {
    tenant_id: String,
    acl: String,
    replay_hash: String,
    summary: String,
    ttl_seconds: i64,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct DecisionShadowScreenReviewStatusRequest {
    tenant_id: String,
    acl: String,
    replay_hash: String,
    approval_id: String,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct DecisionShadowSlaForecastCreateRequest {
    tenant_id: String,
    acl: String,
    sla_id: String,
    forecast_id: String,
    model_version: String,
    opening_artifact_id: Option<String>,
    opening_source_json: Option<String>,
    retention_until_utc: Option<String>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct DecisionShadowSlaForecastIdRequest {
    tenant_id: String,
    acl: String,
    sla_id: String,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct DecisionShadowSlaScoreCreateRequest {
    tenant_id: String,
    acl: String,
    score_id: String,
    sla_forecast_id: String,
    aggregate_score_id: String,
    observation_artifact_id: Option<String>,
    observation_source_json: Option<String>,
    retention_until_utc: Option<String>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct DecisionShadowSlaScoreIdRequest {
    tenant_id: String,
    acl: String,
    score_id: String,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct DecisionShadowSlaForecastScoreRequest {
    tenant_id: String,
    acl: String,
    sla_forecast_id: String,
}

/// Ticket-SLA source bodies carry a full opening or day-end ticket export, so
/// they use their own limit instead of the 16 KiB decision-model request cap.
fn parse_shadow_sla_source_request<T: serde::de::DeserializeOwned>(
    body: &Bytes,
) -> Result<T, axum::response::Response> {
    if body.len() > MAX_SHADOW_SLA_SOURCE_BODY_BYTES {
        return Err((
            axum::http::StatusCode::PAYLOAD_TOO_LARGE,
            Json(serde_json::json!({ "error": "ticket-SLA shadow source request exceeds size limit" })),
        )
            .into_response());
    }
    serde_json::from_slice(body).map_err(|_| {
        (
            axum::http::StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "invalid decision model request" })),
        )
            .into_response()
    })
}

/// Mirrors the 16 KiB `DefaultBodyLimit` on the decision routes. The limit is
/// checked again in the parser because the parser, not the layer, is what runs
/// after authorization.
const MAX_DECISION_MODEL_REQUEST_BYTES: usize = 16 * 1024;
/// Empirical resampling carries a larger sampling plan; its route allows
/// 32 KiB.
const MAX_DECISION_RESAMPLING_REQUEST_BYTES: usize = 32 * 1024;
/// A decision query is at most three short scope fields and a limit.
const MAX_DECISION_QUERY_BYTES: usize = 2 * 1024;

/// Decision requests are read as `Bytes` and deserialized only *after*
/// `authorize_causal_admin`. Axum's `Json<T>` extractor runs before the
/// handler body, so its rejection text (`unknown field ...`, `invalid type:
/// ... expected ...`) let an unauthenticated caller map an admin-only schema
/// and see its own input echoed back. Every decision handler goes through one
/// of these two helpers instead.
fn parse_decision_request_within<T: serde::de::DeserializeOwned>(
    body: &Bytes,
    max_bytes: usize,
) -> Result<T, axum::response::Response> {
    if body.len() > max_bytes {
        return Err((
            axum::http::StatusCode::PAYLOAD_TOO_LARGE,
            Json(serde_json::json!({ "error": "decision model request exceeds size limit" })),
        )
            .into_response());
    }
    serde_json::from_slice(body).map_err(|_| {
        (
            axum::http::StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "invalid decision model request" })),
        )
            .into_response()
    })
}

fn parse_decision_model_request<T: serde::de::DeserializeOwned>(
    body: &Bytes,
) -> Result<T, axum::response::Response> {
    parse_decision_request_within(body, MAX_DECISION_MODEL_REQUEST_BYTES)
}

/// Query strings parsed after authorization, for the same reason as
/// [`parse_decision_request_within`]: `Query<T>` would reject (and name the
/// expected fields) before the handler checks admin rights.
struct DecisionQueryPairs(std::collections::BTreeMap<String, String>);

impl DecisionQueryPairs {
    fn parse(raw: Option<&str>) -> Result<Self, axum::response::Response> {
        let raw = raw.unwrap_or_default();
        if raw.len() > MAX_DECISION_QUERY_BYTES {
            return Err((
                axum::http::StatusCode::PAYLOAD_TOO_LARGE,
                Json(serde_json::json!({ "error": "decision query exceeds size limit" })),
            )
                .into_response());
        }
        let mut pairs = std::collections::BTreeMap::new();
        for (key, value) in url::form_urlencoded::parse(raw.as_bytes()) {
            // A repeated key is ambiguous, never "last one wins".
            if pairs.insert(key.into_owned(), value.into_owned()).is_some() {
                return Err(decision_query_error());
            }
        }
        Ok(Self(pairs))
    }

    fn required(&mut self, key: &str) -> Result<String, axum::response::Response> {
        self.0.remove(key).ok_or_else(decision_query_error)
    }

    fn optional_usize(&mut self, key: &str) -> Result<Option<usize>, axum::response::Response> {
        match self.0.remove(key) {
            None => Ok(None),
            Some(value) => value.parse().map(Some).map_err(|_| decision_query_error()),
        }
    }

    /// Mirror `deny_unknown_fields`: a key nobody consumed is a request the
    /// caller did not mean, not a key to ignore.
    fn finish(self) -> Result<(), axum::response::Response> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(decision_query_error())
        }
    }
}

fn decision_query_error() -> axum::response::Response {
    (
        axum::http::StatusCode::BAD_REQUEST,
        Json(serde_json::json!({ "error": "invalid decision query" })),
    )
        .into_response()
}

fn decision_overview_query(
    raw: Option<&str>,
) -> Result<DecisionOverviewQuery, axum::response::Response> {
    let mut pairs = DecisionQueryPairs::parse(raw)?;
    let query = DecisionOverviewQuery {
        tenant_id: pairs.required("tenant_id")?,
        acl: pairs.required("acl")?,
        limit: pairs.optional_usize("limit")?,
    };
    pairs.finish()?;
    Ok(query)
}

fn decision_catalog_query(
    raw: Option<&str>,
) -> Result<DecisionCatalogQuery, axum::response::Response> {
    let mut pairs = DecisionQueryPairs::parse(raw)?;
    let query = DecisionCatalogQuery {
        tenant_id: pairs.required("tenant_id")?,
        acl: pairs.required("acl")?,
    };
    pairs.finish()?;
    Ok(query)
}

fn decision_shadow_monitor_query(
    raw: Option<&str>,
) -> Result<DecisionShadowMonitorQuery, axum::response::Response> {
    let mut pairs = DecisionQueryPairs::parse(raw)?;
    let query = DecisionShadowMonitorQuery {
        tenant_id: pairs.required("tenant_id")?,
        acl: pairs.required("acl")?,
        policy_id: pairs.required("policy_id")?,
    };
    pairs.finish()?;
    Ok(query)
}

#[derive(serde::Deserialize)]
#[cfg_attr(test, derive(serde::Serialize))]
#[serde(deny_unknown_fields)]
struct DecisionEmpiricalScreenRequest {
    resource_plan: StaffingResourcePlan,
    criteria: JointRiskScreenCriteria,
}

#[derive(serde::Deserialize)]
#[cfg_attr(test, derive(serde::Serialize))]
#[serde(deny_unknown_fields)]
struct DecisionEmpiricalResamplingRequest {
    tenant_id: String,
    acl: String,
    snapshot_id: String,
    model_version: String,
    baseline_scenario_id: String,
    alternative_scenario_id: String,
    training_days: usize,
    min_saturated_days: usize,
    runs: usize,
    arrival_block_days: usize,
    sampling_mode: EmpiricalSamplingMode,
    capacity_fallback_range: Option<BoundedCount>,
    max_final_backlog: u64,
    max_staff_cost_cents: u64,
    min_sla_resolved: Option<u64>,
    screen: Option<DecisionEmpiricalScreenRequest>,
}

#[derive(serde::Deserialize)]
#[cfg_attr(test, derive(serde::Serialize))]
#[serde(deny_unknown_fields)]
struct DecisionSyntheticPilotRequest {
    tenant_id: String,
    acl: String,
    seed: u64,
    days: usize,
}

#[derive(serde::Deserialize)]
#[cfg_attr(test, derive(serde::Serialize))]
#[serde(deny_unknown_fields)]
struct DecisionSyntheticPilotLifecycleRequest {
    tenant_id: String,
    acl: String,
    seed: u64,
    days: usize,
    kind: SyntheticLifecycleKind,
}

#[derive(serde::Deserialize)]
#[cfg_attr(test, derive(serde::Serialize))]
#[serde(deny_unknown_fields)]
struct DecisionCompareRequest {
    tenant_id: String,
    acl: String,
    snapshot_id: String,
    model_version: String,
    baseline_scenario_id: String,
    alternative_scenario_id: String,
    empirical_run_id: Option<String>,
    policy_screen_hash: Option<String>,
    baseline_event_replay_hash: Option<String>,
    alternative_event_replay_hash: Option<String>,
    sla_holdout_id: Option<String>,
    forecast_validation_id: Option<String>,
    #[serde(default)]
    effect_ids: Vec<String>,
}

#[derive(serde::Deserialize)]
#[cfg_attr(test, derive(serde::Serialize))]
#[serde(deny_unknown_fields)]
struct DecisionEventCompareRequest {
    tenant_id: String,
    acl: String,
    snapshot_id: String,
    model_version: String,
    baseline_scenario_id: String,
    alternative_scenario_id: String,
}

#[derive(serde::Deserialize)]
#[cfg_attr(test, derive(serde::Serialize))]
#[serde(deny_unknown_fields)]
struct DecisionReplayRequest {
    tenant_id: String,
    acl: String,
    snapshot_id: String,
    model_version: String,
    scenario_id: String,
    expected_hash: String,
}

#[derive(serde::Deserialize)]
#[cfg_attr(test, derive(serde::Serialize))]
#[serde(deny_unknown_fields)]
struct DecisionPilotReviewRequest {
    tenant_id: String,
    acl: String,
    snapshot_id: String,
    model_version: String,
    scenario_id: String,
    expected_hash: String,
    summary: String,
    ttl_seconds: i64,
}

#[derive(serde::Deserialize)]
#[cfg_attr(test, derive(serde::Serialize))]
#[serde(deny_unknown_fields)]
struct DecisionPilotReviewStatusRequest {
    tenant_id: String,
    acl: String,
    snapshot_id: String,
    model_version: String,
    scenario_id: String,
    expected_hash: String,
    approval_id: String,
}

async fn handle_decision_catalog(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    axum::extract::RawQuery(raw_query): axum::extract::RawQuery,
) -> axum::response::Response {
    if let Err(response) = authorize_causal_admin(&state, &headers) {
        return response;
    }
    let query = match decision_catalog_query(raw_query.as_deref()) {
        Ok(query) => query,
        Err(response) => return response,
    };
    let db = state.home_dir.join("decisions.db");
    let causal_db = state.home_dir.join("memory.db");
    decision_store_response(
        tokio::task::spawn_blocking(move || {
            let store = DecisionStore::with_causal_store(db, CausalStore::new(causal_db));
            let scope = DecisionScope {
                tenant_id: query.tenant_id,
                acl: query.acl,
            };
            Ok(serde_json::to_value(store.dashboard_catalog(&scope)?)?)
        })
        .await,
    )
}

async fn handle_decision_import_pilot(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    if let Err(response) = authorize_causal_admin(&state, &headers) {
        return response;
    }
    if body.len() > MAX_OPERATOR_IMPORT_BODY_BYTES {
        return (
            axum::http::StatusCode::PAYLOAD_TOO_LARGE,
            Json(serde_json::json!({"error":"pilot import exceeds size limit"})),
        )
            .into_response();
    }
    // A serde extraction error can echo an unrecognized attacker-supplied
    // field name. Keep all parse failures and ticket values out of responses.
    let request: OperatorPilotImportRequest = match serde_json::from_slice(&body) {
        Ok(request) => request,
        Err(_) => {
            return (
                axum::http::StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error":"invalid pilot import request"})),
            )
                .into_response();
        }
    };
    let db = state.home_dir.join("decisions.db");
    let causal_db = state.home_dir.join("memory.db");
    decision_store_response(
        tokio::task::spawn_blocking(move || {
            let store = DecisionStore::with_causal_store(db, CausalStore::new(causal_db));
            Ok(serde_json::to_value(
                store.import_operator_pilot(&request)?,
            )?)
        })
        .await,
    )
}

async fn handle_decision_shadow_monitor(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    axum::extract::RawQuery(raw_query): axum::extract::RawQuery,
) -> axum::response::Response {
    if let Err(response) = authorize_causal_admin(&state, &headers) {
        return response;
    }
    let query = match decision_shadow_monitor_query(raw_query.as_deref()) {
        Ok(query) => query,
        Err(response) => return response,
    };
    let db = state.home_dir.join("decisions.db");
    let causal_db = state.home_dir.join("memory.db");
    decision_store_response(
        tokio::task::spawn_blocking(move || {
            let store = DecisionStore::with_causal_store(db, CausalStore::new(causal_db));
            let scope = DecisionScope {
                tenant_id: query.tenant_id,
                acl: query.acl,
            };
            Ok(serde_json::to_value(
                store.dashboard_shadow_monitor(&scope, &query.policy_id)?,
            )?)
        })
        .await,
    )
}

async fn handle_decision_engineering_validation(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    if let Err(response) = authorize_causal_admin(&state, &headers) {
        return response;
    }
    let request: DecisionEngineeringValidationRequest = match parse_decision_model_request(&body) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let db = state.home_dir.join("decisions.db");
    let causal_db = state.home_dir.join("memory.db");
    decision_store_response(
        tokio::task::spawn_blocking(move || {
            let store = DecisionStore::with_causal_store(db, CausalStore::new(causal_db));
            let scope = DecisionScope {
                tenant_id: request.tenant_id,
                acl: request.acl,
            };
            let report = store.dashboard_synthetic_engineering_validation(
                &scope,
                &request.snapshot_id,
                &request.model_version,
            )?;
            Ok(serde_json::json!({ "report": report }))
        })
        .await,
    )
}

async fn handle_decision_forecast_validation(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    if let Err(response) = authorize_causal_admin(&state, &headers) {
        return response;
    }
    let request: DecisionForecastValidationRequest = match parse_decision_model_request(&body) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let db = state.home_dir.join("decisions.db");
    let causal_db = state.home_dir.join("memory.db");
    decision_store_response(
        tokio::task::spawn_blocking(move || {
            let store = DecisionStore::with_causal_store(db, CausalStore::new(causal_db));
            let scope = DecisionScope {
                tenant_id: request.tenant_id,
                acl: request.acl,
            };
            let report = store.dashboard_forecast_validation(
                &scope,
                &request.snapshot_id,
                &request.model_version,
                &request.baseline_scenario_id,
                &request.alternative_scenario_id,
            )?;
            Ok(serde_json::json!({ "report": report }))
        })
        .await,
    )
}

async fn handle_decision_model_candidate_create(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    if let Err(response) = authorize_causal_admin(&state, &headers) {
        return response;
    }
    let request: DecisionModelCandidateCreateRequest = match parse_decision_model_request(&body) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let db = state.home_dir.join("decisions.db");
    let causal_db = state.home_dir.join("memory.db");
    decision_store_response(
        tokio::task::spawn_blocking(move || {
            let store = DecisionStore::with_causal_store(db, CausalStore::new(causal_db));
            let scope = DecisionScope {
                tenant_id: request.tenant_id,
                acl: request.acl,
            };
            let candidate = store.dashboard_create_model_candidate(
                &scope,
                &request.candidate_id,
                &request.screen_replay_hash,
            )?;
            Ok(serde_json::json!({ "candidate": candidate }))
        })
        .await,
    )
}

async fn handle_decision_model_candidate_load(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    if let Err(response) = authorize_causal_admin(&state, &headers) {
        return response;
    }
    let request: DecisionModelCandidateLoadRequest = match parse_decision_model_request(&body) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let db = state.home_dir.join("decisions.db");
    let causal_db = state.home_dir.join("memory.db");
    decision_store_response(
        tokio::task::spawn_blocking(move || {
            let store = DecisionStore::with_causal_store(db, CausalStore::new(causal_db));
            let scope = DecisionScope {
                tenant_id: request.tenant_id,
                acl: request.acl,
            };
            let candidate = store.dashboard_load_model_candidate(&scope, &request.candidate_id)?;
            Ok(serde_json::json!({ "candidate": candidate }))
        })
        .await,
    )
}

async fn handle_decision_model_candidate_compare(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    if let Err(response) = authorize_causal_admin(&state, &headers) {
        return response;
    }
    let request: DecisionModelCandidateCompareRequest = match parse_decision_model_request(&body) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let db = state.home_dir.join("decisions.db");
    let causal_db = state.home_dir.join("memory.db");
    decision_store_response(
        tokio::task::spawn_blocking(move || {
            let store = DecisionStore::with_causal_store(db, CausalStore::new(causal_db));
            let scope = DecisionScope {
                tenant_id: request.tenant_id,
                acl: request.acl,
            };
            let run = store.dashboard_save_model_candidate_run(
                &scope,
                &request.candidate_id,
                &request.target_snapshot_id,
                &request.scenario_id,
            )?;
            Ok(serde_json::json!({ "run": run }))
        })
        .await,
    )
}

async fn handle_decision_model_candidate_load_run(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    if let Err(response) = authorize_causal_admin(&state, &headers) {
        return response;
    }
    let request: DecisionModelCandidateLoadRunRequest = match parse_decision_model_request(&body) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let db = state.home_dir.join("decisions.db");
    let causal_db = state.home_dir.join("memory.db");
    decision_store_response(
        tokio::task::spawn_blocking(move || {
            let store = DecisionStore::with_causal_store(db, CausalStore::new(causal_db));
            let scope = DecisionScope {
                tenant_id: request.tenant_id,
                acl: request.acl,
            };
            let run = store.dashboard_load_model_candidate_run(&scope, &request.replay_hash)?;
            Ok(serde_json::json!({ "run": run }))
        })
        .await,
    )
}

async fn handle_decision_model_candidate_score(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    if let Err(response) = authorize_causal_admin(&state, &headers) {
        return response;
    }
    let request: DecisionModelCandidateScoreRequest = match parse_decision_model_request(&body) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let db = state.home_dir.join("decisions.db");
    let causal_db = state.home_dir.join("memory.db");
    decision_store_response(
        tokio::task::spawn_blocking(move || {
            let store = DecisionStore::with_causal_store(db, CausalStore::new(causal_db));
            let scope = DecisionScope {
                tenant_id: request.tenant_id,
                acl: request.acl,
            };
            let score = store.dashboard_save_model_candidate_score(
                &scope,
                &request.comparison_run_hash,
                &request.outcome_id,
            )?;
            Ok(serde_json::json!({ "score": score }))
        })
        .await,
    )
}

async fn handle_decision_model_candidate_load_score(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    if let Err(response) = authorize_causal_admin(&state, &headers) {
        return response;
    }
    let request: DecisionModelCandidateLoadScoreRequest = match parse_decision_model_request(&body)
    {
        Ok(request) => request,
        Err(response) => return response,
    };
    let db = state.home_dir.join("decisions.db");
    let causal_db = state.home_dir.join("memory.db");
    decision_store_response(
        tokio::task::spawn_blocking(move || {
            let store = DecisionStore::with_causal_store(db, CausalStore::new(causal_db));
            let scope = DecisionScope {
                tenant_id: request.tenant_id,
                acl: request.acl,
            };
            let score = store.dashboard_load_model_candidate_score(&scope, &request.replay_hash)?;
            Ok(serde_json::json!({ "score": score }))
        })
        .await,
    )
}

async fn handle_decision_outcome_fit_create(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    if let Err(response) = authorize_causal_admin(&state, &headers) {
        return response;
    }
    let request: DecisionOutcomeFitCreateRequest = match parse_decision_model_request(&body) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let db = state.home_dir.join("decisions.db");
    let causal_db = state.home_dir.join("memory.db");
    decision_store_response(
        tokio::task::spawn_blocking(move || {
            let store = DecisionStore::with_causal_store(db, CausalStore::new(causal_db));
            let scope = DecisionScope {
                tenant_id: request.tenant_id,
                acl: request.acl,
            };
            let fit = store.dashboard_create_outcome_fit(
                &scope,
                &request.fit_id,
                &request.outcome_id,
                request.min_saturated_days,
                request.training_days,
            )?;
            Ok(serde_json::json!({ "fit": fit }))
        })
        .await,
    )
}

async fn handle_decision_outcome_fit_load(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    if let Err(response) = authorize_causal_admin(&state, &headers) {
        return response;
    }
    let request: DecisionOutcomeFitLoadRequest = match parse_decision_model_request(&body) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let db = state.home_dir.join("decisions.db");
    let causal_db = state.home_dir.join("memory.db");
    decision_store_response(
        tokio::task::spawn_blocking(move || {
            let store = DecisionStore::with_causal_store(db, CausalStore::new(causal_db));
            let scope = DecisionScope {
                tenant_id: request.tenant_id,
                acl: request.acl,
            };
            let fit = store.dashboard_load_outcome_fit(&scope, &request.fit_id)?;
            Ok(serde_json::json!({ "fit": fit }))
        })
        .await,
    )
}

async fn handle_decision_outcome_screen_save(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    if let Err(response) = authorize_causal_admin(&state, &headers) {
        return response;
    }
    let request: DecisionOutcomeScreenSaveRequest = match parse_decision_model_request(&body) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let db = state.home_dir.join("decisions.db");
    let causal_db = state.home_dir.join("memory.db");
    decision_store_response(
        tokio::task::spawn_blocking(move || {
            let store = DecisionStore::with_causal_store(db, CausalStore::new(causal_db));
            let scope = DecisionScope {
                tenant_id: request.tenant_id,
                acl: request.acl,
            };
            let criteria = OutcomeModelReviewCriteria {
                min_saturated_days: request.min_saturated_days,
                min_holdout_days: request.min_holdout_days,
            };
            let screen = store.dashboard_save_outcome_screen(&scope, &request.fit_id, &criteria)?;
            Ok(serde_json::json!({ "screen": screen }))
        })
        .await,
    )
}

async fn handle_decision_outcome_screen_load(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    if let Err(response) = authorize_causal_admin(&state, &headers) {
        return response;
    }
    let request: DecisionOutcomeScreenLoadRequest = match parse_decision_model_request(&body) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let db = state.home_dir.join("decisions.db");
    let causal_db = state.home_dir.join("memory.db");
    decision_store_response(
        tokio::task::spawn_blocking(move || {
            let store = DecisionStore::with_causal_store(db, CausalStore::new(causal_db));
            let scope = DecisionScope {
                tenant_id: request.tenant_id,
                acl: request.acl,
            };
            let screen = store.dashboard_load_outcome_screen(&scope, &request.replay_hash)?;
            Ok(serde_json::json!({ "screen": screen }))
        })
        .await,
    )
}

async fn handle_decision_outcome_screen_review_request(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    let admin = match authorize_causal_admin(&state, &headers) {
        Ok(admin) => admin,
        Err(response) => return response,
    };
    let request: DecisionOutcomeScreenReviewRequest = match parse_decision_model_request(&body) {
        Ok(request) => request,
        Err(response) => return response,
    };
    if request.summary.trim().is_empty()
        || request.summary.len() > 240
        || !(1..=86_400).contains(&request.ttl_seconds)
    {
        return decision_store_response(Ok(Err(DecisionStoreError::Invalid)));
    }
    let home = state.home_dir.clone();
    let result = async {
        let store = DecisionStore::with_causal_store(
            home.join("decisions.db"),
            CausalStore::new(home.join("memory.db")),
        );
        let broker = crate::approval::ApprovalBroker::open(&home)
            .map_err(DecisionStoreError::ReviewBroker)?;
        let scope = DecisionScope {
            tenant_id: request.tenant_id,
            acl: request.acl,
        };
        let summary = format!(
            "Inspect support outcome model screen {} in Decision Lab. {}",
            request.replay_hash,
            request.summary.trim(),
        );
        let link = store
            .request_outcome_model_review(
                &broker,
                &scope,
                &request.replay_hash,
                &admin.user_id,
                &summary,
                request.ttl_seconds,
            )
            .await?;
        let review = store
            .outcome_model_review_status(&broker, &scope, &link.approval_id, &request.replay_hash)
            .await?;
        Ok(serde_json::json!({ "review": review }))
    }
    .await;
    decision_store_response(Ok(result))
}

async fn handle_decision_outcome_screen_review_status(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    if let Err(response) = authorize_causal_admin(&state, &headers) {
        return response;
    }
    let request: DecisionOutcomeScreenReviewStatusRequest =
        match parse_decision_model_request(&body) {
            Ok(request) => request,
            Err(response) => return response,
        };
    let home = state.home_dir.clone();
    let result = async {
        let store = DecisionStore::with_causal_store(
            home.join("decisions.db"),
            CausalStore::new(home.join("memory.db")),
        );
        let broker = crate::approval::ApprovalBroker::open(&home)
            .map_err(DecisionStoreError::ReviewBroker)?;
        let scope = DecisionScope {
            tenant_id: request.tenant_id,
            acl: request.acl,
        };
        let review = store
            .outcome_model_review_status(
                &broker,
                &scope,
                &request.approval_id,
                &request.replay_hash,
            )
            .await?;
        Ok(serde_json::json!({ "review": review }))
    }
    .await;
    decision_store_response(Ok(result))
}

fn shadow_dashboard_response(
    result: Result<Result<serde_json::Value, DecisionStoreError>, tokio::task::JoinError>,
) -> axum::response::Response {
    // Source JSON field names may be operator-controlled. Do not echo parser
    // diagnostics into an HTTP response, even after admin authorization.
    decision_store_response(result.map(|result| {
        result.map_err(|error| match error {
            DecisionStoreError::Json(_) | DecisionStoreError::Observation(_) => {
                DecisionStoreError::Invalid
            }
            other => other,
        })
    }))
}

async fn shadow_dashboard_sync<T, F>(
    state: Arc<AppState>,
    headers: axum::http::HeaderMap,
    body: Bytes,
    action: F,
) -> axum::response::Response
where
    T: serde::de::DeserializeOwned + Send + 'static,
    F: FnOnce(DecisionStore, T) -> Result<serde_json::Value, DecisionStoreError> + Send + 'static,
{
    if let Err(response) = authorize_causal_admin(&state, &headers) {
        return response;
    }
    let request: T = match parse_decision_model_request(&body) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let db = state.home_dir.join("decisions.db");
    let causal_db = state.home_dir.join("memory.db");
    shadow_dashboard_response(
        tokio::task::spawn_blocking(move || {
            let store = DecisionStore::with_causal_store(db, CausalStore::new(causal_db));
            action(store, request)
        })
        .await,
    )
}

async fn handle_decision_shadow_policy_create(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    shadow_dashboard_sync::<DecisionShadowPolicyCreateRequest, _>(
        state,
        headers,
        body,
        |store, request| {
            let scope = DecisionScope {
                tenant_id: request.tenant_id,
                acl: request.acl,
            };
            let policy = store.dashboard_create_shadow_policy(
                &scope,
                &request.policy_id,
                &request.source_lineage,
                &request.queue_id,
                &request.effective_from_utc,
                &request.effective_until_utc,
                request.issue_deadline_seconds,
                request.min_training_days,
                request.min_saturated_days,
            )?;
            Ok(serde_json::json!({"policy":policy}))
        },
    )
    .await
}

async fn handle_decision_shadow_policy_load(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    shadow_dashboard_sync::<DecisionShadowPolicyIdRequest, _>(state, headers, body, |store, request| {
        let scope = DecisionScope { tenant_id: request.tenant_id, acl: request.acl };
        Ok(serde_json::json!({"policy":store.dashboard_load_shadow_policy(&scope, &request.policy_id)?}))
    }).await
}

async fn handle_decision_shadow_forecast_create(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    shadow_dashboard_sync::<DecisionShadowForecastCreateRequest, _>(
        state,
        headers,
        body,
        |store, request| {
            let scope = DecisionScope {
                tenant_id: request.tenant_id,
                acl: request.acl,
            };
            let forecast = store.dashboard_create_shadow_forecast(
                &scope,
                &request.forecast_id,
                &request.policy_id,
                &request.target_day_utc,
                request.known,
                request.training_artifact_id.as_deref(),
                request.training_source_json.as_deref(),
                request.retention_until_utc.as_deref(),
            )?;
            Ok(serde_json::json!({"forecast":forecast}))
        },
    )
    .await
}

async fn handle_decision_shadow_forecast_load(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    shadow_dashboard_sync::<DecisionShadowForecastIdRequest, _>(state, headers, body, |store, request| {
        let scope = DecisionScope { tenant_id: request.tenant_id, acl: request.acl };
        Ok(serde_json::json!({"forecast":store.dashboard_load_shadow_forecast(&scope, &request.forecast_id)?}))
    }).await
}

async fn handle_decision_shadow_score_create(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    shadow_dashboard_sync::<DecisionShadowScoreCreateRequest, _>(
        state,
        headers,
        body,
        |store, request| {
            let scope = DecisionScope {
                tenant_id: request.tenant_id,
                acl: request.acl,
            };
            let score = store.dashboard_create_shadow_score(
                &scope,
                &request.score_id,
                &request.forecast_id,
                request.observation_artifact_id.as_deref(),
                request.observation_source_json.as_deref(),
                request.retention_until_utc.as_deref(),
            )?;
            Ok(serde_json::json!({"score":score}))
        },
    )
    .await
}

async fn handle_decision_shadow_score_load(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    shadow_dashboard_sync::<DecisionShadowScoreIdRequest, _>(state, headers, body, |store, request| {
        let scope = DecisionScope { tenant_id: request.tenant_id, acl: request.acl };
        Ok(serde_json::json!({"score":store.dashboard_load_shadow_score(&scope, &request.score_id)?}))
    }).await
}

async fn handle_decision_shadow_policy_assess(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    shadow_dashboard_sync::<DecisionShadowPolicyIdRequest, _>(state, headers, body, |store, request| {
        let scope = DecisionScope { tenant_id: request.tenant_id, acl: request.acl };
        Ok(serde_json::json!({"assessment":store.dashboard_assess_shadow_policy(&scope, &request.policy_id)?}))
    }).await
}

async fn handle_decision_shadow_screen_evaluate(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    shadow_dashboard_sync::<DecisionShadowScreenCriteriaRequest, _>(state, headers, body, |store, request| {
        let scope = DecisionScope { tenant_id: request.tenant_id, acl: request.acl };
        let criteria = ShadowReviewCriteria {
            min_complete_days: request.min_complete_days,
            min_fixed_coverage_bps: request.min_fixed_coverage_bps,
        };
        Ok(serde_json::json!({"screen":store.dashboard_evaluate_shadow_screen(&scope, &request.policy_id, &criteria)?}))
    }).await
}

async fn handle_decision_shadow_screen_save(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    shadow_dashboard_sync::<DecisionShadowScreenCriteriaRequest, _>(state, headers, body, |store, request| {
        let scope = DecisionScope { tenant_id: request.tenant_id, acl: request.acl };
        let criteria = ShadowReviewCriteria {
            min_complete_days: request.min_complete_days,
            min_fixed_coverage_bps: request.min_fixed_coverage_bps,
        };
        Ok(serde_json::json!({"screen":store.dashboard_save_shadow_screen(&scope, &request.policy_id, &criteria)?}))
    }).await
}

async fn handle_decision_shadow_screen_load(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    shadow_dashboard_sync::<DecisionShadowScreenLoadRequest, _>(state, headers, body, |store, request| {
        let scope = DecisionScope { tenant_id: request.tenant_id, acl: request.acl };
        Ok(serde_json::json!({"screen":store.dashboard_load_shadow_screen(&scope, &request.replay_hash)?}))
    }).await
}

async fn handle_decision_shadow_screen_review_request(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    let admin = match authorize_causal_admin(&state, &headers) {
        Ok(admin) => admin,
        Err(response) => return response,
    };
    let request: DecisionShadowScreenReviewRequest = match parse_decision_model_request(&body) {
        Ok(request) => request,
        Err(response) => return response,
    };
    if request.summary.trim().is_empty()
        || request.summary.len() > 240
        || !(1..=86_400).contains(&request.ttl_seconds)
    {
        return shadow_dashboard_response(Ok(Err(DecisionStoreError::Invalid)));
    }
    let home = state.home_dir.clone();
    let result = async {
        let store = DecisionStore::with_causal_store(
            home.join("decisions.db"),
            CausalStore::new(home.join("memory.db")),
        );
        let broker = crate::approval::ApprovalBroker::open(&home)
            .map_err(DecisionStoreError::ReviewBroker)?;
        let scope = DecisionScope {
            tenant_id: request.tenant_id,
            acl: request.acl,
        };
        let summary = format!(
            "Inspect aggregate shadow screen {} in Decision Lab. {}",
            request.replay_hash,
            request.summary.trim(),
        );
        let link = store
            .request_shadow_screen_review(
                &broker,
                &scope,
                &request.replay_hash,
                &admin.user_id,
                &summary,
                request.ttl_seconds,
            )
            .await?;
        let review = store
            .shadow_screen_review_status(&broker, &scope, &link.approval_id, &request.replay_hash)
            .await?;
        Ok(serde_json::json!({"review":review}))
    }
    .await;
    shadow_dashboard_response(Ok(result))
}

async fn handle_decision_shadow_screen_review_status(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    if let Err(response) = authorize_causal_admin(&state, &headers) {
        return response;
    }
    let request: DecisionShadowScreenReviewStatusRequest = match parse_decision_model_request(&body)
    {
        Ok(request) => request,
        Err(response) => return response,
    };
    let home = state.home_dir.clone();
    let result = async {
        let store = DecisionStore::with_causal_store(
            home.join("decisions.db"),
            CausalStore::new(home.join("memory.db")),
        );
        let broker = crate::approval::ApprovalBroker::open(&home)
            .map_err(DecisionStoreError::ReviewBroker)?;
        let scope = DecisionScope {
            tenant_id: request.tenant_id,
            acl: request.acl,
        };
        let review = store
            .shadow_screen_review_status(
                &broker,
                &scope,
                &request.approval_id,
                &request.replay_hash,
            )
            .await?;
        Ok(serde_json::json!({"review":review}))
    }
    .await;
    shadow_dashboard_response(Ok(result))
}

/// SLA source validators parse operator-supplied exports; their diagnostics may
/// name a rejected field. Collapse every parse-shaped failure to `Invalid` so a
/// source field name or ticket identifier can never reach an HTTP response.
fn sla_shadow_dashboard_response(
    result: Result<Result<serde_json::Value, DecisionStoreError>, tokio::task::JoinError>,
) -> axum::response::Response {
    shadow_dashboard_response(result.map(|result| {
        result.map_err(|error| match error {
            DecisionStoreError::SlaHoldout(_) => DecisionStoreError::Invalid,
            other => other,
        })
    }))
}

async fn shadow_sla_dashboard_sync<T, F>(
    state: Arc<AppState>,
    headers: axum::http::HeaderMap,
    body: Bytes,
    large_source: bool,
    action: F,
) -> axum::response::Response
where
    T: serde::de::DeserializeOwned + Send + 'static,
    F: FnOnce(DecisionStore, T) -> Result<serde_json::Value, DecisionStoreError> + Send + 'static,
{
    if let Err(response) = authorize_causal_admin(&state, &headers) {
        return response;
    }
    let parsed = if large_source {
        parse_shadow_sla_source_request(&body)
    } else {
        parse_decision_model_request(&body)
    };
    let request: T = match parsed {
        Ok(request) => request,
        Err(response) => return response,
    };
    let db = state.home_dir.join("decisions.db");
    let causal_db = state.home_dir.join("memory.db");
    sla_shadow_dashboard_response(
        tokio::task::spawn_blocking(move || {
            let store = DecisionStore::with_causal_store(db, CausalStore::new(causal_db));
            action(store, request)
        })
        .await,
    )
}

async fn handle_decision_shadow_sla_forecast_create(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    shadow_sla_dashboard_sync::<DecisionShadowSlaForecastCreateRequest, _>(
        state,
        headers,
        body,
        true,
        |store, request| {
            let scope = DecisionScope {
                tenant_id: request.tenant_id,
                acl: request.acl,
            };
            let forecast = store.dashboard_create_shadow_sla_forecast(
                &scope,
                &request.sla_id,
                &request.forecast_id,
                &request.model_version,
                request.opening_artifact_id.as_deref(),
                request.opening_source_json.as_deref(),
                request.retention_until_utc.as_deref(),
            )?;
            Ok(serde_json::json!({ "sla_forecast": forecast }))
        },
    )
    .await
}

async fn handle_decision_shadow_sla_forecast_load(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    shadow_sla_dashboard_sync::<DecisionShadowSlaForecastIdRequest, _>(
        state,
        headers,
        body,
        false,
        |store, request| {
            let scope = DecisionScope {
                tenant_id: request.tenant_id,
                acl: request.acl,
            };
            Ok(serde_json::json!({
                "sla_forecast": store.dashboard_load_shadow_sla_forecast(&scope, &request.sla_id)?
            }))
        },
    )
    .await
}

async fn handle_decision_shadow_sla_score_create(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    shadow_sla_dashboard_sync::<DecisionShadowSlaScoreCreateRequest, _>(
        state,
        headers,
        body,
        true,
        |store, request| {
            let scope = DecisionScope {
                tenant_id: request.tenant_id,
                acl: request.acl,
            };
            let score = store.dashboard_create_shadow_sla_score(
                &scope,
                &request.score_id,
                &request.sla_forecast_id,
                &request.aggregate_score_id,
                request.observation_artifact_id.as_deref(),
                request.observation_source_json.as_deref(),
                request.retention_until_utc.as_deref(),
            )?;
            Ok(serde_json::json!({ "sla_score": score }))
        },
    )
    .await
}

async fn handle_decision_shadow_sla_score_load(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    shadow_sla_dashboard_sync::<DecisionShadowSlaScoreIdRequest, _>(
        state,
        headers,
        body,
        false,
        |store, request| {
            let scope = DecisionScope {
                tenant_id: request.tenant_id,
                acl: request.acl,
            };
            Ok(serde_json::json!({
                "sla_score": store.dashboard_load_shadow_sla_score(&scope, &request.score_id)?
            }))
        },
    )
    .await
}

async fn handle_decision_shadow_sla_score_load_current(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    shadow_sla_dashboard_sync::<DecisionShadowSlaForecastScoreRequest, _>(
        state,
        headers,
        body,
        false,
        |store, request| {
            let scope = DecisionScope {
                tenant_id: request.tenant_id,
                acl: request.acl,
            };
            Ok(serde_json::json!({
                "sla_score": store
                    .dashboard_load_current_shadow_sla_score(&scope, &request.sla_forecast_id)?
            }))
        },
    )
    .await
}

async fn handle_decision_shadow_sla_policy_assess(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    shadow_sla_dashboard_sync::<DecisionShadowPolicyIdRequest, _>(
        state,
        headers,
        body,
        false,
        |store, request| {
            let scope = DecisionScope {
                tenant_id: request.tenant_id,
                acl: request.acl,
            };
            Ok(serde_json::json!({
                "assessment": store.dashboard_assess_shadow_sla_policy(&scope, &request.policy_id)?
            }))
        },
    )
    .await
}

async fn handle_decision_sla_shadow_screen_evaluate(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    shadow_sla_dashboard_sync::<DecisionShadowScreenCriteriaRequest, _>(
        state,
        headers,
        body,
        false,
        |store, request| {
            let scope = DecisionScope {
                tenant_id: request.tenant_id,
                acl: request.acl,
            };
            let criteria = ShadowReviewCriteria {
                min_complete_days: request.min_complete_days,
                min_fixed_coverage_bps: request.min_fixed_coverage_bps,
            };
            Ok(serde_json::json!({
                "screen": store.dashboard_evaluate_sla_shadow_screen(
                    &scope,
                    &request.policy_id,
                    &criteria,
                )?
            }))
        },
    )
    .await
}

async fn handle_decision_sla_shadow_screen_save(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    shadow_sla_dashboard_sync::<DecisionShadowScreenCriteriaRequest, _>(
        state,
        headers,
        body,
        false,
        |store, request| {
            let scope = DecisionScope {
                tenant_id: request.tenant_id,
                acl: request.acl,
            };
            let criteria = ShadowReviewCriteria {
                min_complete_days: request.min_complete_days,
                min_fixed_coverage_bps: request.min_fixed_coverage_bps,
            };
            Ok(serde_json::json!({
                "screen": store.dashboard_save_sla_shadow_screen(
                    &scope,
                    &request.policy_id,
                    &criteria,
                )?
            }))
        },
    )
    .await
}

async fn handle_decision_sla_shadow_screen_load(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    shadow_sla_dashboard_sync::<DecisionShadowScreenLoadRequest, _>(
        state,
        headers,
        body,
        false,
        |store, request| {
            let scope = DecisionScope {
                tenant_id: request.tenant_id,
                acl: request.acl,
            };
            Ok(serde_json::json!({
                "screen": store.dashboard_load_sla_shadow_screen(&scope, &request.replay_hash)?
            }))
        },
    )
    .await
}

async fn handle_decision_sla_shadow_screen_review_request(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    let admin = match authorize_causal_admin(&state, &headers) {
        Ok(admin) => admin,
        Err(response) => return response,
    };
    let request: DecisionShadowScreenReviewRequest = match parse_decision_model_request(&body) {
        Ok(request) => request,
        Err(response) => return response,
    };
    if request.summary.trim().is_empty()
        || request.summary.len() > 240
        || !(1..=86_400).contains(&request.ttl_seconds)
    {
        return sla_shadow_dashboard_response(Ok(Err(DecisionStoreError::Invalid)));
    }
    let home = state.home_dir.clone();
    let result = async {
        let store = DecisionStore::with_causal_store(
            home.join("decisions.db"),
            CausalStore::new(home.join("memory.db")),
        );
        let broker = crate::approval::ApprovalBroker::open(&home)
            .map_err(DecisionStoreError::ReviewBroker)?;
        let scope = DecisionScope {
            tenant_id: request.tenant_id,
            acl: request.acl,
        };
        let summary = format!(
            "Inspect ticket-SLA shadow screen {} in Decision Lab. {}",
            request.replay_hash,
            request.summary.trim(),
        );
        let link = store
            .request_sla_shadow_screen_review(
                &broker,
                &scope,
                &request.replay_hash,
                &admin.user_id,
                &summary,
                request.ttl_seconds,
            )
            .await?;
        let review = store
            .sla_shadow_screen_review_status(
                &broker,
                &scope,
                &link.approval_id,
                &request.replay_hash,
            )
            .await?;
        Ok(serde_json::json!({ "review": review }))
    }
    .await;
    sla_shadow_dashboard_response(Ok(result))
}

async fn handle_decision_sla_shadow_screen_review_status(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    if let Err(response) = authorize_causal_admin(&state, &headers) {
        return response;
    }
    let request: DecisionShadowScreenReviewStatusRequest = match parse_decision_model_request(&body)
    {
        Ok(request) => request,
        Err(response) => return response,
    };
    let home = state.home_dir.clone();
    let result = async {
        let store = DecisionStore::with_causal_store(
            home.join("decisions.db"),
            CausalStore::new(home.join("memory.db")),
        );
        let broker = crate::approval::ApprovalBroker::open(&home)
            .map_err(DecisionStoreError::ReviewBroker)?;
        let scope = DecisionScope {
            tenant_id: request.tenant_id,
            acl: request.acl,
        };
        let review = store
            .sla_shadow_screen_review_status(
                &broker,
                &scope,
                &request.approval_id,
                &request.replay_hash,
            )
            .await?;
        Ok(serde_json::json!({ "review": review }))
    }
    .await;
    sla_shadow_dashboard_response(Ok(result))
}

async fn handle_decision_empirical_resampling(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    if let Err(response) = authorize_causal_admin(&state, &headers) {
        return response;
    }
    let request: DecisionEmpiricalResamplingRequest =
        match parse_decision_request_within(&body, MAX_DECISION_RESAMPLING_REQUEST_BYTES) {
            Ok(request) => request,
            Err(response) => return response,
        };
    let db = state.home_dir.join("decisions.db");
    let causal_db = state.home_dir.join("memory.db");
    decision_store_response(
        tokio::task::spawn_blocking(move || {
            let store = DecisionStore::with_causal_store(db, CausalStore::new(causal_db));
            let scope = DecisionScope {
                tenant_id: request.tenant_id,
                acl: request.acl,
            };
            let plan = EmpiricalSensitivityPlan {
                runs: request.runs,
                arrival_block_days: request.arrival_block_days,
                sampling_mode: request.sampling_mode,
                capacity_fallback_range: request.capacity_fallback_range,
                max_final_backlog: request.max_final_backlog,
                max_staff_cost_cents: request.max_staff_cost_cents,
                min_sla_resolved: request.min_sla_resolved,
            };
            let screen = request
                .screen
                .as_ref()
                .map(|input| (&input.resource_plan, &input.criteria));
            Ok(serde_json::to_value(
                store.dashboard_empirical_resampling(
                    &scope,
                    &request.snapshot_id,
                    &request.model_version,
                    &request.baseline_scenario_id,
                    &request.alternative_scenario_id,
                    request.training_days,
                    request.min_saturated_days,
                    &plan,
                    screen,
                )?,
            )?)
        })
        .await,
    )
}

async fn handle_decision_synthetic_pilot(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    if let Err(response) = authorize_causal_admin(&state, &headers) {
        return response;
    }
    let request: DecisionSyntheticPilotRequest = match parse_decision_model_request(&body) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let home = state.home_dir.clone();
    synthetic_adapter_response(
        tokio::task::spawn_blocking(move || {
            let scope = DecisionScope {
                tenant_id: request.tenant_id,
                acl: request.acl,
            };
            Ok(serde_json::to_value(
                SyntheticLocalConnectorAdapter::for_home(home).create_and_bind(
                    &scope,
                    request.seed,
                    request.days,
                )?,
            )?)
        })
        .await,
    )
}

async fn handle_decision_synthetic_pilot_lifecycle(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    if let Err(response) = authorize_causal_admin(&state, &headers) {
        return response;
    }
    let request: DecisionSyntheticPilotLifecycleRequest = match parse_decision_model_request(&body) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let home = state.home_dir.clone();
    synthetic_adapter_response(
        tokio::task::spawn_blocking(move || {
            let scope = DecisionScope {
                tenant_id: request.tenant_id,
                acl: request.acl,
            };
            let outcome = SyntheticLocalConnectorAdapter::for_home(home).stage_event(
                &scope,
                request.seed,
                request.days,
                request.kind,
            )?;
            Ok(serde_json::json!({ "stage_outcome": outcome }))
        })
        .await,
    )
}

async fn handle_decision_compare(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    if let Err(response) = authorize_causal_admin(&state, &headers) {
        return response;
    }
    let request: DecisionCompareRequest = match parse_decision_model_request(&body) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let db = state.home_dir.join("decisions.db");
    let causal_db = state.home_dir.join("memory.db");
    decision_store_response(
        tokio::task::spawn_blocking(move || {
            let store = DecisionStore::with_causal_store(db, CausalStore::new(causal_db));
            let scope = DecisionScope {
                tenant_id: request.tenant_id,
                acl: request.acl,
            };
            if request.policy_screen_hash.is_some() && request.empirical_run_id.is_none() {
                return Err(DecisionStoreError::Invalid);
            }
            let event_evidence = match (
                request.baseline_event_replay_hash.as_deref(),
                request.alternative_event_replay_hash.as_deref(),
            ) {
                (None, None) => None,
                (Some(baseline_hash), Some(alternative_hash))
                    if request.empirical_run_id.is_some() =>
                {
                    let (report, source_bytes) = store.dashboard_event_comparison_with_source(
                        &scope,
                        &request.snapshot_id,
                        &request.model_version,
                        &request.baseline_scenario_id,
                        &request.alternative_scenario_id,
                    )?;
                    if report.baseline.replay_hash != baseline_hash
                        || report.alternative.replay_hash != alternative_hash
                    {
                        return Err(DecisionStoreError::Invalid);
                    }
                    Some((report, source_bytes))
                }
                _ => return Err(DecisionStoreError::Invalid),
            };
            let sla_evidence = request
                .sla_holdout_id
                .as_deref()
                .map(|holdout_id| {
                    store.dashboard_uploaded_sla_holdout_source(
                        &scope,
                        &request.snapshot_id,
                        &request.model_version,
                        &request.baseline_scenario_id,
                        &request.alternative_scenario_id,
                        holdout_id,
                    )
                })
                .transpose()?;
            let forecast_evidence = request
                .forecast_validation_id
                .as_deref()
                .map(|record_id| {
                    store.dashboard_forecast_evidence_source(
                        &scope,
                        &request.snapshot_id,
                        &request.model_version,
                        &request.baseline_scenario_id,
                        &request.alternative_scenario_id,
                        record_id,
                    )
                })
                .transpose()?;
            if let (Some((event_report, event_source)), Some((receipt, sla_source))) =
                (&event_evidence, &sla_evidence)
            {
                if event_source != sla_source || event_report.source_sha256 != receipt.source_sha256
                {
                    return Err(DecisionStoreError::Revoked);
                }
            }
            if let Some(forecast) = &forecast_evidence {
                if event_evidence.as_ref().is_some_and(|(report, source)| {
                    source != &forecast.source_bytes
                        || report.source_sha256 != forecast.source_sha256
                }) || sla_evidence.as_ref().is_some_and(|(receipt, source)| {
                    source != &forecast.source_bytes
                        || receipt.source_sha256 != forecast.source_sha256
                }) {
                    return Err(DecisionStoreError::Revoked);
                }
            }
            let mut brief = store.compare_scenarios_with_evidence(
                &scope,
                &request.snapshot_id,
                &request.model_version,
                &request.baseline_scenario_id,
                &request.alternative_scenario_id,
                BriefEvidenceSelection {
                    empirical_run_id: request.empirical_run_id.as_deref(),
                    policy_screen_hash: request.policy_screen_hash.as_deref(),
                    event_runs: event_evidence.as_ref().map(|(report, source_bytes)| {
                        (
                            report.baseline.replay_hash.as_str(),
                            report.alternative.replay_hash.as_str(),
                            source_bytes.as_slice(),
                        )
                    }),
                    forecast_validation: request.forecast_validation_id.as_deref().zip(
                        forecast_evidence
                            .as_ref()
                            .map(|evidence| evidence.source_bytes.as_slice()),
                    ),
                    sla_holdout: request.sla_holdout_id.as_deref().zip(
                        sla_evidence
                            .as_ref()
                            .map(|(_, source_bytes)| source_bytes.as_slice()),
                    ),
                    effect_ids: &request.effect_ids,
                },
                Vec::new(),
            )?;
            if let Some((report, source_bytes)) = event_evidence {
                let evidence = brief
                    .exploratory_event
                    .as_ref()
                    .ok_or(DecisionStoreError::Corrupt)?;
                store.verify_dashboard_event_evidence_still_current(
                    &scope,
                    &request.snapshot_id,
                    &request.model_version,
                    &request.baseline_scenario_id,
                    &request.alternative_scenario_id,
                    &report,
                    &source_bytes,
                    evidence,
                )?;
            }
            if let Some((receipt, source_bytes)) = sla_evidence {
                let holdout_id = receipt
                    .sla_holdout_id
                    .as_deref()
                    .ok_or(DecisionStoreError::Corrupt)?;
                let evidence = brief
                    .exploratory_sla_holdout
                    .as_ref()
                    .ok_or(DecisionStoreError::Corrupt)?;
                let (current, current_source) = store.dashboard_uploaded_sla_holdout_source(
                    &scope,
                    &request.snapshot_id,
                    &request.model_version,
                    &request.baseline_scenario_id,
                    &request.alternative_scenario_id,
                    holdout_id,
                )?;
                if current != receipt
                    || current_source != source_bytes
                    || evidence.record_id != holdout_id
                    || receipt.sla_holdout_sha256.as_deref()
                        != Some(evidence.record_sha256.as_str())
                    || evidence.source_sha256 != receipt.source_sha256
                {
                    return Err(DecisionStoreError::Revoked);
                }
                brief.limitations.push(
                    "The SLA holdout uses an operator-uploaded export; upstream identity, definitions, and authenticity are unverified".into(),
                );
            }
            if let Some(forecast) = forecast_evidence {
                let record_id = request
                    .forecast_validation_id
                    .as_deref()
                    .ok_or(DecisionStoreError::Corrupt)?;
                let evidence = brief
                    .exploratory_forecast
                    .as_ref()
                    .ok_or(DecisionStoreError::Corrupt)?;
                let current = store.dashboard_forecast_evidence_source(
                    &scope,
                    &request.snapshot_id,
                    &request.model_version,
                    &request.baseline_scenario_id,
                    &request.alternative_scenario_id,
                    record_id,
                )?;
                if current != forecast
                    || evidence.record_id != record_id
                    || evidence.record_sha256 != forecast.record_sha256
                    || evidence.source_sha256 != forecast.source_sha256
                {
                    return Err(DecisionStoreError::Revoked);
                }
                brief.limitations.push(if forecast.operator_receipt.is_some() {
                    "Historical forecast validation uses an operator-uploaded export; upstream identity, definitions, and authenticity are unverified".into()
                } else {
                    "Historical forecast validation uses a synthetic fixture; no real-data calibration is claimed".into()
                });
            }
            Ok(serde_json::json!({ "brief": brief }))
        })
        .await,
    )
}

async fn handle_decision_event_compare(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    if let Err(response) = authorize_causal_admin(&state, &headers) {
        return response;
    }
    let request: DecisionEventCompareRequest = match parse_decision_model_request(&body) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let db = state.home_dir.join("decisions.db");
    let causal_db = state.home_dir.join("memory.db");
    decision_store_response(
        tokio::task::spawn_blocking(move || {
            let store = DecisionStore::with_causal_store(db, CausalStore::new(causal_db));
            let scope = DecisionScope {
                tenant_id: request.tenant_id,
                acl: request.acl,
            };
            let report = store.dashboard_event_comparison(
                &scope,
                &request.snapshot_id,
                &request.model_version,
                &request.baseline_scenario_id,
                &request.alternative_scenario_id,
            )?;
            Ok(serde_json::json!({ "report": report }))
        })
        .await,
    )
}

async fn handle_decision_replay(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    if let Err(response) = authorize_causal_admin(&state, &headers) {
        return response;
    }
    let request: DecisionReplayRequest = match parse_decision_model_request(&body) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let db = state.home_dir.join("decisions.db");
    let causal_db = state.home_dir.join("memory.db");
    decision_store_response(
        tokio::task::spawn_blocking(move || {
            if request.expected_hash.trim().is_empty() {
                return Err(DecisionStoreError::Invalid);
            }
            let store = DecisionStore::with_causal_store(db, CausalStore::new(causal_db));
            let scope = DecisionScope {
                tenant_id: request.tenant_id,
                acl: request.acl,
            };
            let result = store.replay(
                &scope,
                &request.snapshot_id,
                &request.model_version,
                &request.scenario_id,
            )?;
            if result.replay_hash != request.expected_hash {
                return Err(DecisionStoreError::VersionConflict);
            }
            Ok(serde_json::json!({ "result": result }))
        })
        .await,
    )
}

/// Request human inspection of exactly one source-verified pilot run. The
/// persisted run and ApprovalBroker link are receipts only: this route never
/// changes staffing, promotes a model, or decides the approval.
async fn handle_decision_pilot_review_request(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    let admin = match authorize_causal_admin(&state, &headers) {
        Ok(admin) => admin,
        Err(response) => return response,
    };
    let request: DecisionPilotReviewRequest = match parse_decision_model_request(&body) {
        Ok(request) => request,
        Err(response) => return response,
    };
    if request.expected_hash.len() != 64
        || !request
            .expected_hash
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
        || request.summary.trim().is_empty()
        || request.summary.len() > 240
        || !(1..=86_400).contains(&request.ttl_seconds)
    {
        return decision_store_response(Ok(Err(DecisionStoreError::Invalid)));
    }
    let home = state.home_dir.clone();
    let db = home.join("decisions.db");
    let causal_db = home.join("memory.db");
    let scope = DecisionScope {
        tenant_id: request.tenant_id,
        acl: request.acl,
    };
    let snapshot_id = request.snapshot_id;
    let model_version = request.model_version;
    let scenario_id = request.scenario_id;
    let expected_hash = request.expected_hash;
    let summary = request.summary;
    let ttl_seconds = request.ttl_seconds;
    let request_scope = scope.clone();
    let request_db = db.clone();
    let request_causal_db = causal_db.clone();
    let request_snapshot = snapshot_id.clone();
    let request_model = model_version.clone();
    let request_scenario = scenario_id.clone();
    let request_hash = expected_hash.clone();
    let prepared = tokio::task::spawn_blocking(move || {
        let store =
            DecisionStore::with_causal_store(request_db, CausalStore::new(request_causal_db));
        let catalog = store.dashboard_catalog(&request_scope)?;
        let synthetic = catalog.synthetic_pilots.iter().any(|pilot| {
            pilot.snapshot_id == request_snapshot
                && pilot.model_version == request_model
                && (pilot.baseline_scenario_id == request_scenario
                    || pilot.alternative_scenario_id == request_scenario)
        });
        let uploaded = catalog.uploaded_pilots.iter().find(|pilot| {
            pilot.snapshot_id == request_snapshot
                && pilot.model_version == request_model
                && (pilot.baseline_scenario_id == request_scenario
                    || pilot.alternative_scenario_id == request_scenario)
        });
        if !synthetic && uploaded.is_none() {
            return Err(DecisionStoreError::Invalid);
        }
        if synthetic {
            store.dashboard_synthetic_engineering_validation(
                &request_scope,
                &request_snapshot,
                &request_model,
            )?;
        } else if let Some(receipt) = uploaded {
            store.validate_operator_pilot_receipt(&request_scope, receipt)?;
        }
        let replay = store.replay(
            &request_scope,
            &request_snapshot,
            &request_model,
            &request_scenario,
        )?;
        if replay.replay_hash != request_hash {
            return Err(DecisionStoreError::VersionConflict);
        }
        let run = store.put_daily_run(
            &request_scope,
            &request_snapshot,
            &request_model,
            &request_scenario,
        )?;
        if run.replay_hash != request_hash {
            return Err(DecisionStoreError::VersionConflict);
        }
        Ok(())
    })
    .await;
    match prepared {
        Ok(Ok(())) => {}
        Ok(Err(error)) => return decision_store_response(Ok(Err(error))),
        Err(error) => return decision_store_response(Err(error)),
    }
    let result = async {
        let store = DecisionStore::with_causal_store(db, CausalStore::new(causal_db));
        let broker = crate::approval::ApprovalBroker::open(&home)
            .map_err(DecisionStoreError::ReviewBroker)?;
        let approval_summary = format!(
            "Inspect support pilot run {expected_hash} in Decision Lab. Scenario {scenario_id}. {}",
            summary.trim(),
        );
        let link = store
            .request_pilot_review(
                &broker,
                &scope,
                &expected_hash,
                &admin.user_id,
                &approval_summary,
                ttl_seconds,
            )
            .await?;
        let review = store
            .pilot_review_status(
                &broker,
                &scope,
                &link.approval_id,
                &expected_hash,
                &snapshot_id,
                &model_version,
                &scenario_id,
            )
            .await?;
        Ok(serde_json::json!({ "review": review }))
    }
    .await;
    decision_store_response(Ok(result))
}

async fn handle_decision_pilot_review_status(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    if let Err(response) = authorize_causal_admin(&state, &headers) {
        return response;
    }
    let request: DecisionPilotReviewStatusRequest = match parse_decision_model_request(&body) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let home = state.home_dir.clone();
    // Resolve the scoped broker link first. This preserves a wrong-hash
    // conflict without disclosing another tenant's link, and checks revoked
    // source state before the active pilot catalog can omit the pilot.
    {
        let store = DecisionStore::with_causal_store(
            home.join("decisions.db"),
            CausalStore::new(home.join("memory.db")),
        );
        let broker = match crate::approval::ApprovalBroker::open(&home) {
            Ok(broker) => broker,
            Err(error) => {
                return decision_store_response(Ok(Err(DecisionStoreError::ReviewBroker(error))));
            }
        };
        let scope = DecisionScope {
            tenant_id: request.tenant_id.clone(),
            acl: request.acl.clone(),
        };
        if let Err(error) = store
            .pilot_review_status(
                &broker,
                &scope,
                &request.approval_id,
                &request.expected_hash,
                &request.snapshot_id,
                &request.model_version,
                &request.scenario_id,
            )
            .await
        {
            return decision_store_response(Ok(Err(error)));
        }
    }
    let verify_home = home.clone();
    let verify_scope = DecisionScope {
        tenant_id: request.tenant_id.clone(),
        acl: request.acl.clone(),
    };
    let verify_snapshot = request.snapshot_id.clone();
    let verify_model = request.model_version.clone();
    let verify_scenario = request.scenario_id.clone();
    let verify_hash = request.expected_hash.clone();
    let verified = tokio::task::spawn_blocking(move || {
        let store = DecisionStore::with_causal_store(
            verify_home.join("decisions.db"),
            CausalStore::new(verify_home.join("memory.db")),
        );
        let catalog = store.dashboard_catalog(&verify_scope)?;
        let synthetic = catalog.synthetic_pilots.iter().any(|pilot| {
            pilot.snapshot_id == verify_snapshot
                && pilot.model_version == verify_model
                && (pilot.baseline_scenario_id == verify_scenario
                    || pilot.alternative_scenario_id == verify_scenario)
        });
        let uploaded = catalog.uploaded_pilots.iter().find(|pilot| {
            pilot.snapshot_id == verify_snapshot
                && pilot.model_version == verify_model
                && (pilot.baseline_scenario_id == verify_scenario
                    || pilot.alternative_scenario_id == verify_scenario)
        });
        if !synthetic && uploaded.is_none() {
            // The first scoped broker check already validated this exact
            // hash. If its source was revoked while the catalog was read,
            // preserve Revoked (410) instead of reporting a missing pilot.
            store.load_daily_run(&verify_scope, &verify_hash)?;
            return Err(DecisionStoreError::NotFound);
        }
        if synthetic {
            store.dashboard_synthetic_engineering_validation(
                &verify_scope,
                &verify_snapshot,
                &verify_model,
            )?;
        } else if let Some(receipt) = uploaded {
            store.validate_operator_pilot_receipt(&verify_scope, receipt)?;
        }
        Ok::<(), DecisionStoreError>(())
    })
    .await;
    match verified {
        Ok(Ok(())) => {}
        Ok(Err(error)) => return decision_store_response(Ok(Err(error))),
        Err(error) => return decision_store_response(Err(error)),
    }
    // A source or approval can change while the canonical synthetic preflight
    // runs. Return only a freshly rechecked receipt.
    let store = DecisionStore::with_causal_store(
        home.join("decisions.db"),
        CausalStore::new(home.join("memory.db")),
    );
    let broker = match crate::approval::ApprovalBroker::open(&home) {
        Ok(broker) => broker,
        Err(error) => {
            return decision_store_response(Ok(Err(DecisionStoreError::ReviewBroker(error))));
        }
    };
    let scope = DecisionScope {
        tenant_id: request.tenant_id.clone(),
        acl: request.acl.clone(),
    };
    let review = match store
        .pilot_review_status(
            &broker,
            &scope,
            &request.approval_id,
            &request.expected_hash,
            &request.snapshot_id,
            &request.model_version,
            &request.scenario_id,
        )
        .await
    {
        Ok(review) => review,
        Err(error) => return decision_store_response(Ok(Err(error))),
    };
    decision_store_response(Ok(Ok(serde_json::json!({ "review": review }))))
}

#[derive(serde::Deserialize)]
#[cfg_attr(test, derive(serde::Serialize))]
#[serde(deny_unknown_fields)]
struct DecisionPolicySweepRequest {
    tenant_id: String,
    acl: String,
    snapshot_id: String,
    model_version: String,
    baseline_scenario_id: String,
    alternative_scenario_id: String,
    resource_plan: StaffingResourcePlan,
}

#[derive(serde::Deserialize)]
#[cfg_attr(test, derive(serde::Serialize))]
#[serde(deny_unknown_fields)]
struct DecisionSensitivityRequest {
    tenant_id: String,
    acl: String,
    snapshot_id: String,
    model_version: String,
    baseline_scenario_id: String,
    alternative_scenario_id: String,
    runs: usize,
    arrival_delta: u32,
    capacity_min: u32,
    capacity_max: u32,
    max_final_backlog: u64,
    max_staff_cost_cents: u64,
}

async fn handle_decision_policy_sweep(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    if let Err(response) = authorize_causal_admin(&state, &headers) {
        return response;
    }
    let request: DecisionPolicySweepRequest = match parse_decision_model_request(&body) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let db = state.home_dir.join("decisions.db");
    let causal_db = state.home_dir.join("memory.db");
    decision_store_response(
        tokio::task::spawn_blocking(move || {
            let store = DecisionStore::with_causal_store(db, CausalStore::new(causal_db));
            let scope = DecisionScope {
                tenant_id: request.tenant_id,
                acl: request.acl,
            };
            let report = store.sweep_staffing_policy(
                &scope,
                &request.snapshot_id,
                &request.model_version,
                &request.baseline_scenario_id,
                &request.alternative_scenario_id,
                &request.resource_plan,
            )?;
            Ok(serde_json::json!({ "report": report }))
        })
        .await,
    )
}

async fn handle_decision_sensitivity(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    if let Err(response) = authorize_causal_admin(&state, &headers) {
        return response;
    }
    let request: DecisionSensitivityRequest = match parse_decision_model_request(&body) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let db = state.home_dir.join("decisions.db");
    let causal_db = state.home_dir.join("memory.db");
    decision_store_response(
        tokio::task::spawn_blocking(move || {
            let store = DecisionStore::with_causal_store(db, CausalStore::new(causal_db));
            let scope = DecisionScope {
                tenant_id: request.tenant_id,
                acl: request.acl,
            };
            let report = store.dashboard_uniform_sensitivity(
                &scope,
                &request.snapshot_id,
                &request.model_version,
                &request.baseline_scenario_id,
                &request.alternative_scenario_id,
                request.runs,
                request.arrival_delta,
                request.capacity_min,
                request.capacity_max,
                request.max_final_backlog,
                request.max_staff_cost_cents,
            )?;
            Ok(serde_json::json!({ "report": report }))
        })
        .await,
    )
}

#[derive(serde::Deserialize)]
struct CausalClaimsQuery {
    tenant_id: String,
    acl: String,
    review_state: Option<String>,
    limit: Option<usize>,
}

#[derive(serde::Deserialize)]
struct CausalItemQuery {
    tenant_id: String,
    acl: String,
    id: String,
    offset: Option<usize>,
    limit: Option<usize>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CausalSourceRemovalRequest {
    tenant_id: String,
    acl: String,
    artifact_id: String,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CausalClaimReviewRequest {
    tenant_id: String,
    acl: String,
    id: String,
    expected_state: String,
    accept: bool,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CausalClaimReviseRequest {
    tenant_id: String,
    acl: String,
    id: String,
    #[serde(flatten)]
    revision: duduclaw_memory::causal_revision::ClaimRevisionInput,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CausalImportMemoryRequest {
    agent_id: String,
    memory_id: String,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CausalImportWikiRequest {
    agent_id: String,
    page_path: String,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CausalImportSharedWikiRequest {
    page_path: String,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CausalExtractRequest {
    tenant_id: String,
    acl: String,
    artifact_id: String,
    question: String,
}

async fn handle_causal_extract(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Json(request): Json<CausalExtractRequest>,
) -> axum::response::Response {
    if let Err(response) = authorize_causal_admin(&state, &headers) {
        return response;
    }
    let Some(policy) =
        crate::causal_extraction_runner::CausalExtractionPolicy::load(&state.home_dir)
    else {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({ "error": "causal extraction is not configured" })),
        )
            .into_response();
    };
    let scope = EvidenceScope {
        tenant_id: request.tenant_id,
        acl: request.acl,
    };
    if !policy.allows(&scope, policy.provider_id(), policy.model()) {
        return (
            axum::http::StatusCode::FORBIDDEN,
            Json(serde_json::json!({ "error": "source scope is not allowed for extraction" })),
        )
            .into_response();
    }
    let Some(key) = duduclaw_llm::resolve_env_key(policy.provider_id()) else {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({ "error": "extraction provider credentials are unavailable" })),
        )
            .into_response();
    };
    let Some(provider) = duduclaw_llm::providers::build_provider(
        policy.provider_id(),
        duduclaw_llm::ApiAuth::new(key),
    ) else {
        return axum::http::StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let store = CausalStore::new(state.home_dir.join("memory.db"));
    match crate::causal_extraction_runner::extract_candidates_with_provider(
        &store,
        &scope,
        &request.artifact_id,
        &request.question,
        policy.model(),
        provider.as_ref(),
        |scope, provider, model| policy.allows(scope, provider, model),
    )
    .await
    {
        Ok(run) => Json(serde_json::json!({ "run": run })).into_response(),
        Err(crate::causal_extraction_runner::CausalExtractionError::Unauthorized) => {
            axum::http::StatusCode::FORBIDDEN.into_response()
        }
        Err(crate::causal_extraction_runner::CausalExtractionError::Source(
            CausalStoreError::NotFound,
        )) => axum::http::StatusCode::NOT_FOUND.into_response(),
        Err(crate::causal_extraction_runner::CausalExtractionError::Source(
            CausalStoreError::InvalidInput,
        )) => axum::http::StatusCode::BAD_REQUEST.into_response(),
        Err(error) => {
            warn!(error = %error, "causal extraction failed");
            (
                axum::http::StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({ "error": "causal extraction failed" })),
            )
                .into_response()
        }
    }
}

/// Offline extraction scoring. The bundled fixture is explicitly unbound;
/// uploaded cases require a current source in the exact admin-selected scope.
#[derive(serde::Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
enum CausalExtractionEvalRequest {
    BundledSynthetic {
        tenant_id: String,
        acl: String,
    },
    ScopedDataset {
        tenant_id: String,
        acl: String,
        dataset: duduclaw_memory::causal_eval::ExtractionEvalDataset,
        case_artifacts: std::collections::BTreeMap<String, String>,
    },
}

fn causal_extraction_eval_scoped(
    db: std::path::PathBuf,
    scope: &EvidenceScope,
    dataset: &duduclaw_memory::causal_eval::ExtractionEvalDataset,
    case_artifacts: &std::collections::BTreeMap<String, String>,
) -> Result<serde_json::Value, (axum::http::StatusCode, String)> {
    use axum::http::StatusCode;
    if case_artifacts.len() != dataset.cases.len() {
        return Err((
            StatusCode::BAD_REQUEST,
            "case_artifacts must contain exactly one artifact ID per case".into(),
        ));
    }
    let store = CausalStore::new(db);
    let mut unique_artifacts = std::collections::HashSet::new();
    let mut leases = Vec::with_capacity(dataset.cases.len());
    let mut versions = Vec::with_capacity(dataset.cases.len());
    for case in &dataset.cases {
        let artifact_id = case_artifacts.get(&case.id).ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                "case_artifacts is missing a case ID".into(),
            )
        })?;
        if artifact_id.trim().is_empty() || !unique_artifacts.insert(artifact_id.as_str()) {
            return Err((
                StatusCode::BAD_REQUEST,
                "case_artifacts must use distinct nonempty artifact IDs".into(),
            ));
        }
        let metadata = store
            .read_artifact_metadata(scope, artifact_id)
            .map_err(|_| {
                (
                    StatusCode::CONFLICT,
                    "an evaluation source is unavailable in the exact scope".into(),
                )
            })?;
        let (source_text, lease) = store
            .source_text_with_delivery_lease(scope, artifact_id)
            .map_err(|_| {
                (
                    StatusCode::CONFLICT,
                    "an evaluation source is unavailable in the exact scope".into(),
                )
            })?;
        if metadata.lineage_id != case.source_lineage_id
            || metadata.occurred_at != case.occurred_at
            || source_text != case.source_text
            || !lease.still_valid()
        {
            return Err((
                StatusCode::CONFLICT,
                "an evaluation case differs from its exact scoped source".into(),
            ));
        }
        versions.push((artifact_id, metadata.version, metadata.content_sha256));
        leases.push(lease);
    }
    let report = duduclaw_memory::causal_eval::evaluate_extraction(dataset).map_err(|error| {
        (
            StatusCode::BAD_REQUEST,
            error.chars().take(256).collect::<String>(),
        )
    })?;
    // A staged revocation can invalidate a held lease before its writer
    // acquires the exclusive source lock. Recheck the exact source and digest
    // before returning even an aggregate score.
    for ((artifact_id, version, digest), lease) in versions.iter().zip(&leases) {
        if !lease.still_valid()
            || store
                .read_artifact_metadata(scope, artifact_id)
                .map_or(true, |metadata| {
                    metadata.version != version.as_str()
                        || metadata.content_sha256 != digest.as_str()
                })
        {
            return Err((
                StatusCode::CONFLICT,
                "an evaluation source changed during scoring".into(),
            ));
        }
    }
    Ok(serde_json::json!({
        "scope": scope,
        "mode": "scoped_dataset",
        "source_provenance": "active_scoped_artifacts_at_evaluation",
        "family_provenance": "caller_supplied_unverified",
        "report": report,
    }))
}

async fn handle_causal_extraction_eval(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Json(request): Json<CausalExtractionEvalRequest>,
) -> axum::response::Response {
    use axum::http::StatusCode;
    if let Err(response) = authorize_causal_admin(&state, &headers) {
        return response;
    }
    let (scope, dataset, case_artifacts) = match request {
        CausalExtractionEvalRequest::BundledSynthetic { tenant_id, acl } => (
            EvidenceScope { tenant_id, acl },
            serde_json::from_str::<duduclaw_memory::causal_eval::ExtractionEvalDataset>(
                include_str!("../../../fixtures/causal-extraction-synthetic.json"),
            )
            .expect("checked-in synthetic extraction fixture"),
            None,
        ),
        CausalExtractionEvalRequest::ScopedDataset {
            tenant_id,
            acl,
            dataset,
            case_artifacts,
        } => (
            EvidenceScope { tenant_id, acl },
            dataset,
            Some(case_artifacts),
        ),
    };
    if scope.tenant_id.trim().is_empty()
        || scope.acl.trim().is_empty()
        || scope.tenant_id.trim() != scope.tenant_id
        || scope.acl.trim() != scope.acl
        || scope.tenant_id.len() > 128
        || scope.acl.len() > 128
        || dataset.cases.len() > 128
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "invalid evaluation scope or case count" })),
        )
            .into_response();
    }
    let db = state.home_dir.join("memory.db");
    match tokio::task::spawn_blocking(move || {
        if let Some(bindings) = case_artifacts {
            causal_extraction_eval_scoped(db, &scope, &dataset, &bindings)
        } else {
            let report =
                duduclaw_memory::causal_eval::evaluate_extraction(&dataset).map_err(|error| {
                    (
                        StatusCode::BAD_REQUEST,
                        error.chars().take(256).collect::<String>(),
                    )
                })?;
            Ok(serde_json::json!({
                "scope": scope,
                "mode": "bundled_synthetic",
                "source_provenance": "bundled_synthetic_unbound",
                "family_provenance": "bundled_synthetic_annotations",
                "report": report,
            }))
        }
    })
    .await
    {
        Ok(Ok(value)) => Json(value).into_response(),
        Ok(Err((status, error))) => {
            (status, Json(serde_json::json!({ "error": error }))).into_response()
        }
        Err(error) => {
            warn!(error = %error, "causal extraction evaluation task failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": "causal evaluation unavailable" })),
            )
                .into_response()
        }
    }
}

async fn handle_causal_claims(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Query(query): Query<CausalClaimsQuery>,
) -> axum::response::Response {
    if let Err(response) = authorize_causal_admin(&state, &headers) {
        return response;
    }
    let db = state.home_dir.join("memory.db");
    causal_store_response(
        tokio::task::spawn_blocking(move || {
            let store = CausalStore::new(db);
            let scope = EvidenceScope {
                tenant_id: query.tenant_id,
                acl: query.acl,
            };
            let ids = store.list_claim_ids(
                &scope,
                query.review_state.as_deref(),
                query.limit.unwrap_or(50),
            )?;
            // One connection for up to 100 claims. A `read_claim` per id used
            // to open the store per id, re-running schema setup and a full
            // live-Wiki resync (every imported page re-read from disk) 100
            // times for one listing.
            let conn = store.read_connection()?;
            let claims = ids
                .iter()
                .map(|id| CausalStore::read_claim_with_conn(&conn, &scope, id))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(serde_json::json!({ "claims": claims }))
        })
        .await,
    )
}

async fn handle_causal_claim(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Query(query): Query<CausalItemQuery>,
) -> axum::response::Response {
    if let Err(response) = authorize_causal_admin(&state, &headers) {
        return response;
    }
    let db = state.home_dir.join("memory.db");
    causal_store_response(tokio::task::spawn_blocking(move || {
        let store = CausalStore::new(db);
        let scope = EvidenceScope { tenant_id: query.tenant_id, acl: query.acl };
        let conn = store.read_connection()?;
        let claim = CausalStore::read_claim_with_conn(&conn, &scope, &query.id)?;
        let state = store.claim_state(&scope, &query.id)?;
        let evidence = store.evidence_for_claim(&scope, &query.id)?;
        let lineages = store.claim_lineage_summary(&scope, &query.id)?;
        let revisions = store.claim_revisions(&scope, &query.id)?;
        Ok(serde_json::json!({ "claim": claim, "effective_state": state, "evidence": evidence, "lineages": lineages, "revisions": revisions }))
    }).await)
}

fn bounded_causal_source(content: &str, offset: usize, limit: usize) -> serde_json::Value {
    let mut start = offset.min(content.len());
    while !content.is_char_boundary(start) {
        start += 1;
    }
    let mut end = start
        .saturating_add(limit.clamp(4, 64 * 1024))
        .min(content.len());
    while end > start && !content.is_char_boundary(end) {
        end -= 1;
    }
    serde_json::json!({
        "text": &content[start..end],
        "byte_offset": start,
        "next_offset": end,
        "total_bytes": content.len(),
        "truncated": end < content.len(),
    })
}

async fn handle_causal_source(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Query(query): Query<CausalItemQuery>,
) -> axum::response::Response {
    if let Err(response) = authorize_causal_admin(&state, &headers) {
        return response;
    }
    let db = state.home_dir.join("memory.db");
    causal_store_response(
        tokio::task::spawn_blocking(move || {
            let store = CausalStore::new(db);
            let scope = EvidenceScope {
                tenant_id: query.tenant_id,
                acl: query.acl,
            };
            let content = store.source_text(&scope, &query.id)?;
            Ok(bounded_causal_source(
                &content,
                query.offset.unwrap_or(0),
                query.limit.unwrap_or(8192),
            ))
        })
        .await,
    )
}

async fn remove_causal_source_admin(
    state: Arc<AppState>,
    headers: axum::http::HeaderMap,
    request: CausalSourceRemovalRequest,
    removal: CausalSourceRemoval,
) -> axum::response::Response {
    if let Err(response) = authorize_causal_admin(&state, &headers) {
        return response;
    }
    let home = state.home_dir.clone();
    decision_store_response(
        tokio::task::spawn_blocking(move || {
            let causal_db = home.join("memory.db");
            let store = DecisionStore::with_causal_store(
                home.join("decisions.db"),
                CausalStore::new(causal_db),
            );
            // The decision store may not yet exist for a causal-only install.
            // Create/check it before mutating the causal or CCR databases.
            store.open()?;
            let ccr_db = home.join("ccr").join("ccr.db");
            let scope = DecisionScope {
                tenant_id: request.tenant_id,
                acl: request.acl,
            };
            let result = store.remove_causal_artifact_with_dependents(
                &scope,
                &request.artifact_id,
                removal,
                Some(ccr_db.as_path()),
            )?;
            Ok(serde_json::json!({ "result": result }))
        })
        .await,
    )
}

async fn handle_causal_source_invalidate(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Json(request): Json<CausalSourceRemovalRequest>,
) -> axum::response::Response {
    remove_causal_source_admin(state, headers, request, CausalSourceRemoval::Invalidate).await
}

async fn handle_causal_source_erase(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Json(request): Json<CausalSourceRemovalRequest>,
) -> axum::response::Response {
    remove_causal_source_admin(state, headers, request, CausalSourceRemoval::Erase).await
}

/// POST /api/causal/source/clear-revocation-fence — admin-only recovery for a
/// revocation that stopped at its fence.
///
/// Revocation hides a source from readers the moment it starts, and that is
/// intentional. This does not undo a revocation that took effect: the store
/// refuses unless the artifact is still intact, no tombstone notice is queued
/// for CCR, and no delivery lease for that version remains. It exists so an
/// abandoned revoke — one that failed on a live lease and was never retried —
/// stops hiding a live source forever.
async fn handle_causal_clear_revocation_fence(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Json(request): Json<CausalSourceRemovalRequest>,
) -> axum::response::Response {
    let admin = match authorize_causal_admin(&state, &headers) {
        Ok(admin) => admin,
        Err(response) => return response,
    };
    let home = state.home_dir.clone();
    let db = home.join("memory.db");
    let audit_scope = format!("{}/{}", request.tenant_id, request.acl);
    let audit_artifact = request.artifact_id.clone();
    let outcome = tokio::task::spawn_blocking(move || {
        let store = CausalStore::new(db);
        let scope = EvidenceScope {
            tenant_id: request.tenant_id,
            acl: request.acl,
        };
        store
            .clear_revocation_fence(&scope, &request.artifact_id)
            .map(|version| serde_json::json!({ "cleared_version": version }))
    })
    .await;
    // Audit before answering: restoring a hidden source is an operator act,
    // and the row must exist whether or not the store allowed it.
    let (ok, error) = match &outcome {
        Ok(Ok(_)) => (true, None),
        Ok(Err(error)) => (false, Some(error.to_string())),
        Err(_) => (false, Some("task join failure".to_string())),
    };
    crate::security_autopilot::audit_and_emit(
        &state.home_dir,
        &duduclaw_security::audit::AuditEvent::new(
            "causal_clear_revocation_fence",
            &audit_artifact,
            duduclaw_security::audit::Severity::Warning,
            serde_json::json!({
                "scope": audit_scope,
                "artifact_id": audit_artifact,
                "actor": admin.email,
                "ok": ok,
                "error": error,
            }),
        ),
    );
    causal_store_response(outcome)
}

async fn handle_causal_import_memory(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Json(request): Json<CausalImportMemoryRequest>,
) -> axum::response::Response {
    if let Err(response) = authorize_causal_admin(&state, &headers) {
        return response;
    }
    let db = state.home_dir.join("memory.db");
    causal_store_response(
        tokio::task::spawn_blocking(move || {
            let store = CausalStore::new(db);
            let source = store.import_memory_source(&request.agent_id, &request.memory_id)?;
            Ok(serde_json::json!({ "source": source }))
        })
        .await,
    )
}

async fn handle_causal_import_wiki(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Json(request): Json<CausalImportWikiRequest>,
) -> axum::response::Response {
    if let Err(response) = authorize_causal_admin(&state, &headers) {
        return response;
    }
    let db = state.home_dir.join("memory.db");
    causal_store_response(
        tokio::task::spawn_blocking(move || {
            let store = CausalStore::new(db);
            let source = store.import_agent_wiki_source(&request.agent_id, &request.page_path)?;
            Ok(serde_json::json!({ "source": source }))
        })
        .await,
    )
}

async fn handle_causal_import_shared_wiki(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Json(request): Json<CausalImportSharedWikiRequest>,
) -> axum::response::Response {
    if let Err(response) = authorize_causal_admin(&state, &headers) {
        return response;
    }
    let db = state.home_dir.join("memory.db");
    causal_store_response(
        tokio::task::spawn_blocking(move || {
            let store = CausalStore::new(db);
            let source = store.import_shared_wiki_source(&request.page_path)?;
            Ok(serde_json::json!({ "source": source }))
        })
        .await,
    )
}

async fn handle_causal_claim_review(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Json(request): Json<CausalClaimReviewRequest>,
) -> axum::response::Response {
    let reviewer = match authorize_causal_admin(&state, &headers) {
        Ok(ctx) => ctx.user_id,
        Err(response) => return response,
    };
    let db = state.home_dir.join("memory.db");
    causal_store_response(
        tokio::task::spawn_blocking(move || {
            let store = CausalStore::new(db);
            let scope = EvidenceScope {
                tenant_id: request.tenant_id,
                acl: request.acl,
            };
            store.review_claim_if_state(
                &scope,
                &request.id,
                &reviewer,
                request.accept,
                &request.expected_state,
            )?;
            let claim = store.read_claim(&scope, &request.id)?;
            Ok(serde_json::json!({ "claim": claim }))
        })
        .await,
    )
}

async fn handle_causal_claim_revise(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Json(request): Json<CausalClaimReviseRequest>,
) -> axum::response::Response {
    let reviewer = match authorize_causal_admin(&state, &headers) {
        Ok(ctx) => ctx.user_id,
        Err(response) => return response,
    };
    let db = state.home_dir.join("memory.db");
    causal_store_response(
        tokio::task::spawn_blocking(move || {
            let store = CausalStore::new(db);
            let scope = EvidenceScope {
                tenant_id: request.tenant_id,
                acl: request.acl,
            };
            let revision = store.revise_claim(&scope, &request.id, &reviewer, &request.revision)?;
            let claim = store.read_claim(&scope, &revision.new_claim_id)?;
            Ok(serde_json::json!({ "revision": revision, "claim": claim }))
        })
        .await,
    )
}

#[derive(serde::Deserialize)]
struct CausalModelsQuery {
    tenant_id: String,
    acl: String,
    limit: Option<usize>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CausalModelReviewRequest {
    tenant_id: String,
    acl: String,
    id: String,
    expected_state: String,
    expected_opposition_digest: String,
    approve: bool,
    acknowledge_conflicts: bool,
}

#[derive(serde::Deserialize)]
struct CausalAssumptionsQuery {
    tenant_id: String,
    acl: String,
    model_id: String,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CausalAssumptionReviewRequest {
    tenant_id: String,
    acl: String,
    model_id: String,
    kind: duduclaw_memory::causal_model::AssumptionKind,
    verdict: duduclaw_memory::causal_model::AssumptionVerdict,
    rationale: String,
    expected_review_id: Option<String>,
}

fn causal_assumption_readiness_json(
    readiness: duduclaw_memory::causal_model::EffectReadiness,
) -> serde_json::Value {
    match readiness {
        duduclaw_memory::causal_model::EffectReadiness::ReadyForEstimator => {
            serde_json::json!({ "state": "ready_for_estimator" })
        }
        duduclaw_memory::causal_model::EffectReadiness::Unknown { reasons } => {
            serde_json::json!({ "state": "unknown", "reasons": reasons })
        }
    }
}

async fn handle_causal_assumptions(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Query(query): Query<CausalAssumptionsQuery>,
) -> axum::response::Response {
    if let Err(response) = authorize_causal_admin(&state, &headers) {
        return response;
    }
    let db = state.home_dir.join("memory.db");
    causal_store_response(
        tokio::task::spawn_blocking(move || {
            let store = CausalStore::new(db);
            let scope = EvidenceScope {
                tenant_id: query.tenant_id,
                acl: query.acl,
            };
            let reviews = store.current_assumption_reviews(&scope, &query.model_id)?;
            let readiness = store.effect_readiness(&scope, &query.model_id)?;
            Ok(serde_json::json!({ "reviews": reviews,
            "readiness": causal_assumption_readiness_json(readiness) }))
        })
        .await,
    )
}

async fn handle_causal_assumption_review(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Json(request): Json<CausalAssumptionReviewRequest>,
) -> axum::response::Response {
    let reviewer = match authorize_causal_admin(&state, &headers) {
        Ok(ctx) => ctx.user_id,
        Err(response) => return response,
    };
    let db = state.home_dir.join("memory.db");
    causal_store_response(
        tokio::task::spawn_blocking(move || {
            let store = CausalStore::new(db);
            let scope = EvidenceScope {
                tenant_id: request.tenant_id,
                acl: request.acl,
            };
            let review_id = store.record_assumption_if_review(
                &scope,
                &request.model_id,
                request.kind,
                request.verdict,
                &request.rationale,
                &reviewer,
                request.expected_review_id.as_deref(),
            )?;
            Ok(serde_json::json!({ "review_id": review_id,
            "readiness": causal_assumption_readiness_json(
                store.effect_readiness(&scope, &request.model_id)?) }))
        })
        .await,
    )
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CausalNegativeControlProtocolRequest {
    tenant_id: String,
    acl: String,
    external_id: String,
    version: String,
    lineage_id: String,
    content: String,
    occurred_at: i64,
    retention_at: i64,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CausalNegativeControlReviewRequest {
    tenant_id: String,
    acl: String,
    model_id: String,
    variable_id: String,
    protocol_artifact_id: String,
    verdict: duduclaw_memory::causal_model::AssumptionVerdict,
    rationale: String,
    expected_review_id: Option<String>,
}

#[derive(serde::Deserialize)]
struct CausalNegativeControlReviewQuery {
    tenant_id: String,
    acl: String,
    model_id: String,
    variable_id: String,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CausalEffectEstimateRequest {
    tenant_id: String,
    acl: String,
    model_id: String,
    external_id: String,
    version: String,
    lineage_id: String,
    occurred_at: i64,
    retention_at: i64,
    dataset: duduclaw_memory::causal_effect::ObservedDataset,
}

fn causal_effect_json(result: duduclaw_memory::causal_effect::EffectResult) -> serde_json::Value {
    match result {
        duduclaw_memory::causal_effect::EffectResult::Estimated(estimate) => {
            serde_json::json!({ "state": "estimated", "estimate": estimate })
        }
        duduclaw_memory::causal_effect::EffectResult::Unknown { reasons } => {
            serde_json::json!({ "state": "unknown", "reasons": reasons })
        }
    }
}

async fn handle_causal_negative_control_protocol(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Json(request): Json<CausalNegativeControlProtocolRequest>,
) -> axum::response::Response {
    if let Err(response) = authorize_causal_admin(&state, &headers) {
        return response;
    }
    let db = state.home_dir.join("memory.db");
    causal_store_response(
        tokio::task::spawn_blocking(move || {
            let store = CausalStore::new(db);
            let scope = EvidenceScope {
                tenant_id: request.tenant_id,
                acl: request.acl,
            };
            let artifact = store.add_artifact(
                &scope,
                "negative_control_protocol",
                &request.external_id,
                &request.version,
                &request.lineage_id,
                &request.content,
                request.occurred_at,
                request.retention_at,
            )?;
            Ok(serde_json::json!({ "artifact": artifact }))
        })
        .await,
    )
}

async fn handle_causal_negative_control_review(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Json(request): Json<CausalNegativeControlReviewRequest>,
) -> axum::response::Response {
    let reviewer = match authorize_causal_admin(&state, &headers) {
        Ok(ctx) => ctx.user_id,
        Err(response) => return response,
    };
    let db = state.home_dir.join("memory.db");
    causal_store_response(
        tokio::task::spawn_blocking(move || {
            let store = CausalStore::new(db);
            let scope = EvidenceScope {
                tenant_id: request.tenant_id,
                acl: request.acl,
            };
            let review_id = store.review_negative_control_if_review(
                &scope,
                &request.model_id,
                &request.variable_id,
                &request.protocol_artifact_id,
                request.verdict,
                &request.rationale,
                &reviewer,
                request.expected_review_id.as_deref(),
            )?;
            Ok(serde_json::json!({ "review_id": review_id }))
        })
        .await,
    )
}

async fn handle_causal_negative_control_review_get(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Query(query): Query<CausalNegativeControlReviewQuery>,
) -> axum::response::Response {
    if let Err(response) = authorize_causal_admin(&state, &headers) {
        return response;
    }
    let db = state.home_dir.join("memory.db");
    causal_store_response(tokio::task::spawn_blocking(move || {
        let store = CausalStore::new(db);
        let scope = EvidenceScope { tenant_id: query.tenant_id, acl: query.acl };
        let review = store.latest_negative_control_review(
            &scope, &query.model_id, &query.variable_id,
        )?;
        let readiness = if let Some(ref review) = review {
            match store.negative_control_readiness(
                &scope, &query.model_id, &query.variable_id, &review.id,
            )? {
                duduclaw_memory::causal_negative_control::NegativeControlReadiness::Reviewed { .. } =>
                    serde_json::json!({ "state": "reviewed" }),
                duduclaw_memory::causal_negative_control::NegativeControlReadiness::Unknown { reasons } =>
                    serde_json::json!({ "state": "unknown", "reasons": reasons }),
            }
        } else {
            serde_json::json!({ "state": "unknown", "reasons": ["review missing"] })
        };
        Ok(serde_json::json!({ "review": review, "readiness": readiness }))
    }).await)
}

async fn handle_causal_effect_estimate(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Json(request): Json<CausalEffectEstimateRequest>,
) -> axum::response::Response {
    if let Err(response) = authorize_causal_admin(&state, &headers) {
        return response;
    }
    let db = state.home_dir.join("memory.db");
    causal_store_response(
        tokio::task::spawn_blocking(move || {
            let store = CausalStore::new(db);
            let scope = EvidenceScope {
                tenant_id: request.tenant_id,
                acl: request.acl,
            };
            store.model_state(&scope, &request.model_id)?;
            let content = serde_json::to_string(&request.dataset)
                .map_err(|_| CausalStoreError::InvalidInput)?;
            let artifact = store.add_artifact(
                &scope,
                "causal_dataset",
                &request.external_id,
                &request.version,
                &request.lineage_id,
                &content,
                request.occurred_at,
                request.retention_at,
            )?;
            let result =
                store.estimate_stratified_effect(&scope, &request.model_id, &artifact.id)?;
            Ok(serde_json::json!({ "dataset_artifact_id": artifact.id,
            "result": causal_effect_json(result) }))
        })
        .await,
    )
}

async fn handle_causal_effect(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Query(query): Query<CausalItemQuery>,
) -> axum::response::Response {
    if let Err(response) = authorize_causal_admin(&state, &headers) {
        return response;
    }
    let db = state.home_dir.join("memory.db");
    causal_store_response(
        tokio::task::spawn_blocking(move || {
            let store = CausalStore::new(db);
            let scope = EvidenceScope {
                tenant_id: query.tenant_id,
                acl: query.acl,
            };
            Ok(causal_effect_json(
                store.read_effect_estimate(&scope, &query.id)?,
            ))
        })
        .await,
    )
}

async fn handle_causal_models(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Query(query): Query<CausalModelsQuery>,
) -> axum::response::Response {
    if let Err(response) = authorize_causal_admin(&state, &headers) {
        return response;
    }
    let db = state.home_dir.join("memory.db");
    causal_store_response(
        tokio::task::spawn_blocking(move || {
            let store = CausalStore::new(db);
            let scope = EvidenceScope {
                tenant_id: query.tenant_id,
                acl: query.acl,
            };
            // Two `open()`s per model became two reads on one connection.
            let conn = store.read_connection()?;
            let models = store
                .list_model_ids(&scope, query.limit.unwrap_or(50))?
                .into_iter()
                .map(|id| {
                    Ok(serde_json::json!({
                        "model": CausalStore::read_model_with_conn(&conn, &scope, &id)?,
                        "effective_state": CausalStore::model_state_with_conn(&conn, &scope, &id)?,
                    }))
                })
                .collect::<Result<Vec<_>, CausalStoreError>>()?;
            Ok(serde_json::json!({ "models": models }))
        })
        .await,
    )
}

async fn handle_causal_model(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Query(query): Query<CausalItemQuery>,
) -> axum::response::Response {
    if let Err(response) = authorize_causal_admin(&state, &headers) {
        return response;
    }
    let db = state.home_dir.join("memory.db");
    causal_store_response(
        tokio::task::spawn_blocking(move || {
            let store = CausalStore::new(db);
            let scope = EvidenceScope {
                tenant_id: query.tenant_id,
                acl: query.acl,
            };
            let review = store.model_review_view(&scope, &query.id)?;
            let parent_adjustment =
                match store.suggest_parent_backdoor_adjustment(&scope, &query.id)? {
                    AdjustmentReadiness::Unknown { reasons } => {
                        serde_json::json!({ "status": "unknown", "reasons": reasons })
                    }
                    AdjustmentReadiness::GraphicallyAdmissible {
                        adjustment_variable_ids,
                    } => serde_json::json!({
                        "status": "graphically_admissible",
                        "adjustment_variable_ids": adjustment_variable_ids,
                    }),
                };
            Ok(serde_json::json!({ "review": review, "parent_adjustment": parent_adjustment }))
        })
        .await,
    )
}

async fn handle_causal_model_review(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Json(request): Json<CausalModelReviewRequest>,
) -> axum::response::Response {
    let reviewer = match authorize_causal_admin(&state, &headers) {
        Ok(ctx) => ctx.user_id,
        Err(response) => return response,
    };
    let db = state.home_dir.join("memory.db");
    causal_store_response(
        tokio::task::spawn_blocking(move || {
            let store = CausalStore::new(db);
            let scope = EvidenceScope {
                tenant_id: request.tenant_id,
                acl: request.acl,
            };
            store.review_model_if_state(
                &scope,
                &request.id,
                &reviewer,
                request.approve,
                request.acknowledge_conflicts,
                &request.expected_state,
                &request.expected_opposition_digest,
            )?;
            Ok(serde_json::json!({ "review": store.model_review_view(&scope, &request.id)? }))
        })
        .await,
    )
}

#[derive(serde::Deserialize)]
struct CausalAliasesQuery {
    tenant_id: String,
    acl: String,
    limit: Option<usize>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CausalAliasSetRequest {
    tenant_id: String,
    acl: String,
    alias: String,
    canonical_name: String,
    expected_review_id: Option<String>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CausalAliasRevokeRequest {
    tenant_id: String,
    acl: String,
    alias: String,
    expected_review_id: String,
}

async fn handle_causal_aliases(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Query(query): Query<CausalAliasesQuery>,
) -> axum::response::Response {
    if let Err(response) = authorize_causal_admin(&state, &headers) {
        return response;
    }
    let db = state.home_dir.join("memory.db");
    causal_store_response(tokio::task::spawn_blocking(move || {
        let store = CausalStore::new(db);
        let scope = EvidenceScope { tenant_id: query.tenant_id, acl: query.acl };
        Ok(serde_json::json!({ "aliases": store.list_variable_aliases(&scope, query.limit.unwrap_or(50))? }))
    }).await)
}

async fn handle_causal_alias_set(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Json(request): Json<CausalAliasSetRequest>,
) -> axum::response::Response {
    let reviewer = match authorize_causal_admin(&state, &headers) {
        Ok(ctx) => ctx.user_id,
        Err(response) => return response,
    };
    let db = state.home_dir.join("memory.db");
    causal_store_response(
        tokio::task::spawn_blocking(move || {
            let store = CausalStore::new(db);
            let scope = EvidenceScope {
                tenant_id: request.tenant_id,
                acl: request.acl,
            };
            let alias = store.set_variable_alias_if_review(
                &scope,
                &request.alias,
                &request.canonical_name,
                &reviewer,
                request.expected_review_id.as_deref(),
            )?;
            Ok(serde_json::json!({ "alias": alias }))
        })
        .await,
    )
}

async fn handle_causal_alias_revoke(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Json(request): Json<CausalAliasRevokeRequest>,
) -> axum::response::Response {
    let reviewer = match authorize_causal_admin(&state, &headers) {
        Ok(ctx) => ctx.user_id,
        Err(response) => return response,
    };
    let db = state.home_dir.join("memory.db");
    causal_store_response(
        tokio::task::spawn_blocking(move || {
            let store = CausalStore::new(db);
            let scope = EvidenceScope {
                tenant_id: request.tenant_id,
                acl: request.acl,
            };
            store.revoke_variable_alias_if_review(
                &scope,
                &request.alias,
                &reviewer,
                &request.expected_review_id,
            )?;
            Ok(serde_json::json!({ "revoked": true }))
        })
        .await,
    )
}

#[cfg(test)]
mod decision_shadow_api_tests {
    use super::*;
    use crate::decision_calibration::ObservedSupportDay;
    use crate::decision_store::ObservedOutcomeExport;
    use duduclaw_auth::UserRole;
    use duduclaw_memory::causal::EvidenceScope;
    use rusqlite::{Connection, params};
    use sha2::{Digest, Sha256};

    fn body(value: serde_json::Value) -> Bytes {
        Bytes::from(serde_json::to_vec(&value).unwrap())
    }

    async fn response_json(response: axum::response::Response) -> serde_json::Value {
        serde_json::from_slice(
            &axum::body::to_bytes(response.into_body(), 64 * 1024)
                .await
                .unwrap(),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn shadow_http_binds_prospective_sources_and_review_states() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();
        let user_db = Arc::new(UserDb::new(&home.join("users.db")).unwrap());
        let admin = user_db
            .create_user(
                "shadow-admin@example.test",
                "Admin",
                "test-password",
                UserRole::Admin,
            )
            .unwrap();
        let employee = user_db
            .create_user(
                "shadow-employee@example.test",
                "Employee",
                "test-password",
                UserRole::Employee,
            )
            .unwrap();
        let jwt_config = Arc::new(JwtConfig::new(b"shadow-api-test-secret-32-bytes!!!!"));
        let admin_token = jwt_config.issue_access_token(&admin, &[]).unwrap();
        let employee_token = jwt_config.issue_access_token(&employee, &[]).unwrap();
        let (tx, _) = broadcast::channel(4);
        let (event_tx, _) = broadcast::channel(4);
        let state = Arc::new(AppState {
            auth: AuthManager::new(None),
            handler: MethodHandler::new(home.clone()).await,
            tx,
            event_tx,
            user_db,
            jwt_config,
            otp_delivery: Arc::new(crate::otp_delivery::ConfigOtpDeliverer::new(
                home.clone(),
                reqwest::Client::new(),
            )),
            home_dir: home.clone(),
        });
        let mut admin_headers = axum::http::HeaderMap::new();
        admin_headers.insert(
            "authorization",
            format!("Bearer {admin_token}").parse().unwrap(),
        );
        admin_headers.insert("origin", "http://localhost:18789".parse().unwrap());
        let mut employee_headers = admin_headers.clone();
        employee_headers.insert(
            "authorization",
            format!("Bearer {employee_token}").parse().unwrap(),
        );
        let causal = CausalStore::new(home.join("memory.db"));
        let store = DecisionStore::with_causal_store(home.join("decisions.db"), causal.clone());
        let scope = DecisionScope {
            tenant_id: "tenant-a".into(),
            acl: "private".into(),
        };
        let evidence_scope = EvidenceScope {
            tenant_id: scope.tenant_id.clone(),
            acl: scope.acl.clone(),
        };
        let midnight = chrono::Utc::now()
            .date_naive()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc();
        let future_start = midnight + chrono::Duration::days(1);
        let policy_create = || {
            serde_json::json!({
                "tenant_id":"tenant-a", "acl":"private", "policy_id":"future-policy",
                "source_lineage":"future-support-lineage", "queue_id":"support-queue",
                "effective_from_utc":future_start.to_rfc3339_opts(chrono::SecondsFormat::Secs,true),
                "effective_until_utc":(future_start + chrono::Duration::days(22)).to_rfc3339_opts(chrono::SecondsFormat::Secs,true),
                "issue_deadline_seconds":3600, "min_training_days":14, "min_saturated_days":7,
            })
        };
        let denied = handle_decision_shadow_policy_create(
            State(state.clone()),
            employee_headers,
            body(policy_create()),
        )
        .await;
        assert_eq!(denied.status(), axum::http::StatusCode::FORBIDDEN);
        let unknown = handle_decision_shadow_policy_create(
            State(state.clone()), admin_headers.clone(), body(serde_json::json!({
                "tenant_id":"tenant-a", "acl":"private", "policy_id":"unknown-policy",
                "source_lineage":"future-support-lineage", "queue_id":"support-queue",
                "effective_from_utc":future_start.to_rfc3339_opts(chrono::SecondsFormat::Secs,true),
                "effective_until_utc":(future_start + chrono::Duration::days(22)).to_rfc3339_opts(chrono::SecondsFormat::Secs,true),
                "issue_deadline_seconds":3600, "min_training_days":14, "min_saturated_days":7,
                "ticket_secret":"secret-ticket-id",
            })),
        ).await;
        assert_eq!(unknown.status(), axum::http::StatusCode::BAD_REQUEST);
        assert!(
            !response_json(unknown)
                .await
                .to_string()
                .contains("secret-ticket-id")
        );
        let future_policy = handle_decision_shadow_policy_create(
            State(state.clone()),
            admin_headers.clone(),
            body(policy_create()),
        )
        .await;
        assert_eq!(future_policy.status(), axum::http::StatusCode::OK);
        let future_policy = response_json(future_policy).await;
        assert_eq!(future_policy["policy"]["queue_id"], "support-queue");
        let future_policy_repeat = response_json(
            handle_decision_shadow_policy_create(
                State(state.clone()),
                admin_headers.clone(),
                body(policy_create()),
            )
            .await,
        )
        .await;
        assert_eq!(future_policy_repeat, future_policy);
        let wrong_policy_scope = handle_decision_shadow_policy_load(
            State(state.clone()),
            admin_headers.clone(),
            body(serde_json::json!({
                "tenant_id":"other", "acl":"private", "policy_id":"future-policy",
            })),
        )
        .await;
        assert_eq!(
            wrong_policy_scope.status(),
            axum::http::StatusCode::NOT_FOUND
        );

        // Test-only historical time seam: production request bodies cannot
        // specify these timestamps or backdate a forecast commitment.
        let start = midnight - chrono::Duration::days(30);
        let training_start = start - chrono::Duration::days(14);
        let end = start + chrono::Duration::days(21);
        let stamp = |time: chrono::DateTime<chrono::Utc>| {
            time.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
        };
        let historical = store
            .shadow_test_policy_at(
                &scope,
                "historical-shadow",
                "support-lineage",
                "support-queue",
                &stamp(start),
                &stamp(end),
                3600,
                14,
                7,
                start.timestamp() - 86_400,
            )
            .unwrap();
        let causal_conn = Connection::open(causal.path()).unwrap();
        let retained_until = chrono::Utc::now() + chrono::Duration::days(30);
        let mut backlog = 100_u64;
        let mut history = Vec::new();
        for _ in 0..14 {
            history.push(ObservedSupportDay {
                arrivals: 20,
                backlog_start: backlog,
                resolved: 16,
                backlog_end: backlog + 4,
                agents: 2,
                fixed_extra_capacity: 0,
            });
            backlog += 4;
        }
        let mut last_forecast_id = String::new();
        let mut last_training_sha256 = String::new();
        let mut last_observation_json = String::new();
        for index in 0..21 {
            let target = start + chrono::Duration::days(index);
            let day_end = target + chrono::Duration::days(1);
            let agents = if index % 2 == 0 { 2 } else { 3 };
            let training = ObservedOutcomeExport {
                queue_id: Some("support-queue".into()),
                window_start_utc: stamp(training_start),
                observed_through_utc: stamp(target),
                observed_days: history.clone(),
                sla_days: None,
                resolved_within_sla_by_day: None,
            };
            let training_json = serde_json::to_string(&training).unwrap();
            let artifact = causal
                .add_artifact(
                    &evidence_scope,
                    "shadow_training_export",
                    &format!("training-{index}"),
                    "v1",
                    "support-lineage",
                    &training_json,
                    target.timestamp(),
                    retained_until.timestamp(),
                )
                .unwrap();
            causal_conn
                .execute(
                    "UPDATE causal_artifacts SET ingested_at=?1 WHERE id=?2",
                    params![target.timestamp() + 60, artifact.id],
                )
                .unwrap();
            let forecast = store
                .shadow_test_forecast_at(
                    &scope,
                    &format!("forecast-{index}"),
                    &artifact.id,
                    &stamp(target),
                    KnownDayInputs {
                        opening_backlog: backlog,
                        planned_agents: agents,
                        planned_fixed_extra_capacity: 0,
                    },
                    &historical.id,
                    target.timestamp() + 120,
                )
                .unwrap();
            let resolved = agents * 8;
            let observed = ObservedSupportDay {
                arrivals: 20,
                backlog_start: backlog,
                resolved,
                backlog_end: backlog + 20 - u64::from(resolved),
                agents,
                fixed_extra_capacity: 0,
            };
            let observation = ObservedOutcomeExport {
                queue_id: Some("support-queue".into()),
                window_start_utc: stamp(target),
                observed_through_utc: stamp(day_end),
                observed_days: vec![observed.clone()],
                sla_days: None,
                resolved_within_sla_by_day: None,
            };
            let observation_json = serde_json::to_string(&observation).unwrap();
            if index < 20 {
                let artifact = causal
                    .add_artifact(
                        &evidence_scope,
                        "shadow_observation_export",
                        &format!("observed-{index}"),
                        "v1",
                        "support-lineage",
                        &observation_json,
                        day_end.timestamp(),
                        retained_until.timestamp(),
                    )
                    .unwrap();
                causal_conn
                    .execute(
                        "UPDATE causal_artifacts SET ingested_at=?1 WHERE id=?2",
                        params![day_end.timestamp() + 1, artifact.id],
                    )
                    .unwrap();
                store
                    .shadow_test_score_at(
                        &scope,
                        &format!("score-{index}"),
                        &forecast.id,
                        &artifact.id,
                        day_end.timestamp() + 2,
                    )
                    .unwrap();
            } else {
                last_forecast_id = forecast.id;
                last_training_sha256 = forecast.training_sha256;
                last_observation_json = observation_json;
            }
            backlog = observed.backlog_end;
            history.push(observed);
        }
        let loaded_forecast = handle_decision_shadow_forecast_load(
            State(state.clone()),
            admin_headers.clone(),
            body(serde_json::json!({
                "tenant_id":"tenant-a", "acl":"private", "forecast_id":last_forecast_id,
            })),
        )
        .await;
        assert_eq!(loaded_forecast.status(), axum::http::StatusCode::OK);
        let loaded_forecast = response_json(loaded_forecast).await;
        assert!(loaded_forecast["forecast"]["known"]["opening_backlog"].is_string());
        assert!(loaded_forecast["forecast"]["forecast"]["predicted_backlog_end"].is_string());
        assert!(loaded_forecast["forecast"].get("observed_days").is_none());
        let training_id = loaded_forecast["forecast"]["training_artifact_id"]
            .as_str()
            .unwrap();
        let training_source = causal.source_text(&evidence_scope, training_id).unwrap();
        let forecast_retry = serde_json::json!({
            "tenant_id":"tenant-a", "acl":"private", "forecast_id":last_forecast_id,
            "policy_id":"historical-shadow",
            "target_day_utc":loaded_forecast["forecast"]["target_day_utc"],
            "known":{
                "opening_backlog":loaded_forecast["forecast"]["known"]["opening_backlog"]
                    .as_str().unwrap().parse::<u64>().unwrap(),
                "planned_agents":loaded_forecast["forecast"]["known"]["planned_agents"],
                "planned_fixed_extra_capacity":loaded_forecast["forecast"]["known"]["planned_fixed_extra_capacity"],
            },
            "training_source_json":training_source,
            "retention_until_utc":stamp(retained_until.clone()),
        });
        // A completed exact retry remains readable after its historical
        // issue deadline; changed inputs are never accepted as a retry.
        let retry_result = handle_decision_shadow_forecast_create(
            State(state.clone()),
            admin_headers.clone(),
            body(forecast_retry.clone()),
        )
        .await;
        assert_eq!(retry_result.status(), axum::http::StatusCode::OK);
        assert_eq!(
            response_json(retry_result).await["forecast"],
            loaded_forecast["forecast"]
        );
        let mut changed_known = forecast_retry.clone();
        changed_known["known"]["planned_agents"] = serde_json::json!(4);
        let rejected_known = handle_decision_shadow_forecast_create(
            State(state.clone()),
            admin_headers.clone(),
            body(changed_known),
        )
        .await;
        assert_eq!(rejected_known.status(), axum::http::StatusCode::CONFLICT);
        let mut changed_retention = forecast_retry.clone();
        changed_retention["retention_until_utc"] =
            serde_json::json!(stamp(retained_until + chrono::Duration::seconds(1)));
        let rejected_retention = handle_decision_shadow_forecast_create(
            State(state.clone()),
            admin_headers.clone(),
            body(changed_retention),
        )
        .await;
        assert_eq!(
            rejected_retention.status(),
            axum::http::StatusCode::CONFLICT
        );
        let late_forecast = handle_decision_shadow_forecast_create(
            State(state.clone()), admin_headers.clone(), body(serde_json::json!({
                "tenant_id":"tenant-a", "acl":"private", "forecast_id":"late-replay",
                "policy_id":"historical-shadow", "target_day_utc":stamp(end-chrono::Duration::days(1)),
                "known":{"opening_backlog":backlog,"planned_agents":2,"planned_fixed_extra_capacity":0},
                "training_artifact_id":loaded_forecast["forecast"]["training_artifact_id"],
            })),
        ).await;
        assert_eq!(late_forecast.status(), axum::http::StatusCode::BAD_REQUEST);
        let incomplete = response_json(
            handle_decision_shadow_policy_assess(
                State(state.clone()),
                admin_headers.clone(),
                body(serde_json::json!({
                    "tenant_id":"tenant-a", "acl":"private", "policy_id":"historical-shadow",
                })),
            )
            .await,
        )
        .await;
        assert_eq!(incomplete["assessment"]["complete"], false);
        assert_eq!(incomplete["assessment"]["day_status_counts"]["unscored"], 1);
        assert_eq!(
            incomplete["assessment"]["error_sums"],
            serde_json::Value::Null
        );
        let criteria = || {
            serde_json::json!({
                "tenant_id":"tenant-a", "acl":"private", "policy_id":"historical-shadow",
                "min_complete_days":21, "min_fixed_coverage_bps":0,
            })
        };
        let preview = response_json(
            handle_decision_shadow_screen_evaluate(
                State(state.clone()),
                admin_headers.clone(),
                body(criteria()),
            )
            .await,
        )
        .await;
        assert_eq!(
            preview["screen"]["report"]["eligible_for_human_review"],
            false
        );
        assert_eq!(preview["screen"]["record_sha256"], serde_json::Value::Null);
        let premature_screen = handle_decision_shadow_screen_save(
            State(state.clone()),
            admin_headers.clone(),
            body(criteria()),
        )
        .await;
        assert_eq!(
            premature_screen.status(),
            axum::http::StatusCode::BAD_REQUEST
        );
        let early_artifact = causal
            .add_artifact(
                &evidence_scope,
                "shadow_observation_export",
                "early-outcome",
                "v1",
                "support-lineage",
                &last_observation_json,
                end.timestamp(),
                i64::MAX,
            )
            .unwrap();
        causal_conn
            .execute(
                "UPDATE causal_artifacts SET ingested_at=?1 WHERE id=?2",
                params![end.timestamp() - 60, early_artifact.id],
            )
            .unwrap();
        let early_score = handle_decision_shadow_score_create(
            State(state.clone()),
            admin_headers.clone(),
            body(serde_json::json!({
                "tenant_id":"tenant-a", "acl":"private", "score_id":"early-score",
                "forecast_id":last_forecast_id, "observation_artifact_id":early_artifact.id,
            })),
        )
        .await;
        assert_eq!(early_score.status(), axum::http::StatusCode::BAD_REQUEST);
        let score_create = serde_json::json!({
            "tenant_id":"tenant-a", "acl":"private", "score_id":"final-score",
            "forecast_id":last_forecast_id, "observation_source_json":last_observation_json,
            "retention_until_utc":stamp(chrono::Utc::now()+chrono::Duration::days(30)),
        });
        // Both callers ingest before either commits. A test-only failure
        // sends A through the same rollback branch while B still holds its
        // distinct, uncommitted source artifact.
        //
        // The ready waits below are deadlock guards, not timing assertions:
        // under a full parallel `--lib` run (~7k tests) the two ingests can
        // sit behind SQLite busy waits for well over 10s, which is exactly
        // how this test flaked on 2026-09-29. 60s keeps the guard and stops
        // the false negative.
        let (a_ready_tx, a_ready_rx) = std::sync::mpsc::channel();
        let (b_ready_tx, b_ready_rx) = std::sync::mpsc::channel();
        let (release_a_tx, release_a_rx) = std::sync::mpsc::channel();
        let (release_b_tx, release_b_rx) = std::sync::mpsc::channel();
        let spawn_score = |store: DecisionStore,
                           scope: DecisionScope,
                           forecast_id: String,
                           source: String,
                           retention: String,
                           ready: std::sync::mpsc::Sender<String>,
                           release: std::sync::mpsc::Receiver<()>,
                           inject_failure: bool| {
            std::thread::spawn(move || {
                store.dashboard_test_create_shadow_score_after_source(
                    &scope,
                    "final-score",
                    &forecast_id,
                    &source,
                    &retention,
                    |artifact_id| {
                        ready.send(artifact_id.to_owned()).unwrap();
                        release.recv().unwrap();
                        if inject_failure {
                            Err(DecisionStoreError::VersionConflict)
                        } else {
                            Ok(())
                        }
                    },
                )
            })
        };
        let retention = score_create["retention_until_utc"]
            .as_str()
            .unwrap()
            .to_owned();
        let contender_a = spawn_score(
            store.clone(),
            scope.clone(),
            last_forecast_id.clone(),
            last_observation_json.clone(),
            retention.clone(),
            a_ready_tx,
            release_a_rx,
            true,
        );
        // Start B only once A is parked at its ready point (2026-09-29, same
        // fix as the SLA interleaving test on 09-28): launching both at once
        // let them race for the SQLite write lock during the source insert,
        // and under a full parallel `--lib` run the loser blew through the
        // 5s busy timeout and died silently, so the ready wait below timed
        // out. The property under test — B ingests while A still holds its
        // uncommitted source — is unchanged; only the racy start is gone.
        let source_a = a_ready_rx
            .recv_timeout(std::time::Duration::from_secs(60))
            .unwrap();
        let contender_b = spawn_score(
            store.clone(),
            scope.clone(),
            last_forecast_id.clone(),
            last_observation_json.clone(),
            retention,
            b_ready_tx,
            release_b_rx,
            false,
        );
        let source_b = b_ready_rx
            .recv_timeout(std::time::Duration::from_secs(60))
            .unwrap();
        assert_ne!(source_a, source_b);
        release_a_tx.send(()).unwrap();
        assert!(matches!(
            contender_a.join().unwrap(),
            Err(DecisionStoreError::VersionConflict)
        ));
        let (loser_content, loser_invalidated): (String, Option<i64>) = causal_conn
            .query_row(
                "SELECT content,invalidated_at FROM causal_artifacts WHERE id=?1",
                [&source_a],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert!(loser_content.is_empty());
        assert!(loser_invalidated.is_some());
        // The surviving source is still in flight when A's cleanup ends.
        assert_eq!(
            causal.source_text(&evidence_scope, &source_b).unwrap(),
            last_observation_json
        );
        release_b_tx.send(()).unwrap();
        let winner = contender_b.join().unwrap().unwrap();
        assert_eq!(winner.observation_artifact_id, source_b);
        let scored = handle_decision_shadow_score_create(
            State(state.clone()),
            admin_headers.clone(),
            body(score_create.clone()),
        )
        .await;
        assert_eq!(scored.status(), axum::http::StatusCode::OK);
        let scored = response_json(scored).await;
        assert!(scored["score"]["backlog_abs_error"].is_string());
        assert!(scored["score"].get("observed").is_none());
        let original_artifact_id = scored["score"]["observation_artifact_id"].as_str().unwrap();
        assert_eq!(original_artifact_id, source_b);
        // A sequential retry returns the exact completed record without
        // creating or erasing another source.
        let reused = handle_decision_shadow_score_create(
            State(state.clone()),
            admin_headers.clone(),
            body(score_create.clone()),
        )
        .await;
        assert_eq!(reused.status(), axum::http::StatusCode::OK);
        assert_eq!(response_json(reused).await, scored);
        assert_eq!(
            causal
                .source_text(&evidence_scope, original_artifact_id)
                .unwrap(),
            last_observation_json
        );

        let mut rejected_source: serde_json::Value =
            serde_json::from_str(&last_observation_json).unwrap();
        rejected_source["observed_days"][0]["arrivals"] = serde_json::json!(21);
        let prior_backlog = rejected_source["observed_days"][0]["backlog_end"]
            .as_u64()
            .unwrap();
        rejected_source["observed_days"][0]["backlog_end"] = serde_json::json!(prior_backlog + 1);
        let rejected_source_text = serde_json::to_string(&rejected_source).unwrap();
        let rejected_digest = format!("{:x}", Sha256::digest(rejected_source_text.as_bytes()));
        let mut changed_source = score_create.clone();
        changed_source["observation_source_json"] = serde_json::json!(rejected_source_text);
        let rejected_same_id = handle_decision_shadow_score_create(
            State(state.clone()),
            admin_headers.clone(),
            body(changed_source),
        )
        .await;
        assert_eq!(rejected_same_id.status(), axum::http::StatusCode::CONFLICT);
        let mut rejected_request = score_create.clone();
        rejected_request["score_id"] = serde_json::json!("duplicate-new-source");
        rejected_request["observation_source_json"] = serde_json::json!(rejected_source_text);
        let rejected = handle_decision_shadow_score_create(
            State(state.clone()),
            admin_headers.clone(),
            body(rejected_request),
        )
        .await;
        assert_eq!(rejected.status(), axum::http::StatusCode::CONFLICT);
        let (scrubbed, invalidated_at): (String, Option<i64>) = causal_conn
            .query_row(
                "SELECT content,invalidated_at FROM causal_artifacts
                 WHERE tenant_id=?1 AND acl=?2 AND kind='shadow_observation_export'
                 AND external_id LIKE 'duplicate-new-source:%' AND version=?3",
                params![scope.tenant_id, scope.acl, rejected_digest],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert!(scrubbed.is_empty());
        assert!(invalidated_at.is_some());
        assert_eq!(
            causal
                .source_text(&evidence_scope, original_artifact_id)
                .unwrap(),
            last_observation_json
        );
        let loaded_score = response_json(
            handle_decision_shadow_score_load(
                State(state.clone()),
                admin_headers.clone(),
                body(serde_json::json!({
                    "tenant_id":"tenant-a", "acl":"private", "score_id":"final-score",
                })),
            )
            .await,
        )
        .await;
        assert_eq!(loaded_score, scored);
        let assessment = response_json(
            handle_decision_shadow_policy_assess(
                State(state.clone()),
                admin_headers.clone(),
                body(serde_json::json!({
                    "tenant_id":"tenant-a", "acl":"private", "policy_id":"historical-shadow",
                })),
            )
            .await,
        )
        .await;
        assert_eq!(assessment["assessment"]["complete"], true);
        assert_eq!(assessment["assessment"]["scored_days"], 21);
        assert!(assessment["assessment"]["error_sums"]["backlog"].is_string());
        assert!(assessment["assessment"]["fixed_prefix_interval"]["covered_points"].is_number());
        assert!(assessment["assessment"].get("days").is_none());
        assert!(
            assessment["assessment"]["fixed_prefix_interval"]
                .get("points")
                .is_none()
        );
        let saved_screen = handle_decision_shadow_screen_save(
            State(state.clone()),
            admin_headers.clone(),
            body(criteria()),
        )
        .await;
        assert_eq!(saved_screen.status(), axum::http::StatusCode::OK);
        let saved_screen = response_json(saved_screen).await;
        assert_eq!(
            saved_screen["screen"]["report"]["eligible_for_human_review"],
            true
        );
        let screen_hash = saved_screen["screen"]["replay_hash"]
            .as_str()
            .unwrap()
            .to_owned();
        let loaded_screen = response_json(
            handle_decision_shadow_screen_load(
                State(state.clone()),
                admin_headers.clone(),
                body(serde_json::json!({
                    "tenant_id":"tenant-a", "acl":"private", "replay_hash":screen_hash,
                })),
            )
            .await,
        )
        .await;
        assert_eq!(loaded_screen, saved_screen);
        let request_review = || {
            serde_json::json!({
                "tenant_id":"tenant-a", "acl":"private", "replay_hash":screen_hash,
                "summary":"Inspect synthetic aggregate shadow history", "ttl_seconds":3600,
            })
        };
        let requested = handle_decision_shadow_screen_review_request(
            State(state.clone()),
            admin_headers.clone(),
            body(request_review()),
        )
        .await;
        assert_eq!(requested.status(), axum::http::StatusCode::OK);
        let requested = response_json(requested).await;
        assert_eq!(requested["review"]["status"], "pending");
        let approval_id = requested["review"]["link"]["approval_id"]
            .as_str()
            .unwrap()
            .to_owned();
        let review_status = || {
            serde_json::json!({
                "tenant_id":"tenant-a", "acl":"private", "replay_hash":screen_hash,
                "approval_id":approval_id,
            })
        };
        let wrong_scope = handle_decision_shadow_screen_review_status(
            State(state.clone()),
            admin_headers.clone(),
            body(serde_json::json!({
                "tenant_id":"other", "acl":"private", "replay_hash":screen_hash,
                "approval_id":approval_id,
            })),
        )
        .await;
        assert_eq!(wrong_scope.status(), axum::http::StatusCode::NOT_FOUND);
        let broker = crate::approval::ApprovalBroker::open(&home).unwrap();
        broker
            .decide(
                &crate::approval::ApprovalId::from(approval_id.clone()),
                true,
                "human",
            )
            .await
            .unwrap();
        let approved = response_json(
            handle_decision_shadow_screen_review_status(
                State(state.clone()),
                admin_headers.clone(),
                body(review_status()),
            )
            .await,
        )
        .await;
        assert_eq!(approved["review"]["status"], "approved");
        let denied_link = response_json(
            handle_decision_shadow_screen_review_request(
                State(state.clone()),
                admin_headers.clone(),
                body(request_review()),
            )
            .await,
        )
        .await;
        let denied_id = denied_link["review"]["link"]["approval_id"]
            .as_str()
            .unwrap()
            .to_owned();
        broker
            .decide(
                &crate::approval::ApprovalId::from(denied_id.clone()),
                false,
                "human",
            )
            .await
            .unwrap();
        let denied_status = response_json(
            handle_decision_shadow_screen_review_status(
                State(state.clone()),
                admin_headers.clone(),
                body(serde_json::json!({
                    "tenant_id":"tenant-a", "acl":"private", "replay_hash":screen_hash,
                    "approval_id":denied_id,
                })),
            )
            .await,
        )
        .await;
        assert_eq!(denied_status["review"]["status"], "denied");
        let expiring = response_json(
            handle_decision_shadow_screen_review_request(
                State(state.clone()),
                admin_headers.clone(),
                body(serde_json::json!({
                    "tenant_id":"tenant-a", "acl":"private", "replay_hash":screen_hash,
                    "summary":"Expire synthetic inspection", "ttl_seconds":1,
                })),
            )
            .await,
        )
        .await;
        let expiring_id = expiring["review"]["link"]["approval_id"]
            .as_str()
            .unwrap()
            .to_owned();
        tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
        let expired = response_json(
            handle_decision_shadow_screen_review_status(
                State(state.clone()),
                admin_headers.clone(),
                body(serde_json::json!({
                    "tenant_id":"tenant-a", "acl":"private", "replay_hash":screen_hash,
                    "approval_id":expiring_id,
                })),
            )
            .await,
        )
        .await;
        assert_eq!(expired["review"]["status"], "expired");
        store
            .revoke_source_version(&scope, &last_training_sha256)
            .unwrap();
        let revoked_forecast = handle_decision_shadow_forecast_load(
            State(state.clone()),
            admin_headers.clone(),
            body(serde_json::json!({
                "tenant_id":"tenant-a", "acl":"private", "forecast_id":last_forecast_id,
            })),
        )
        .await;
        assert_eq!(revoked_forecast.status(), axum::http::StatusCode::GONE);
        let revoked_screen = handle_decision_shadow_screen_load(
            State(state.clone()),
            admin_headers.clone(),
            body(serde_json::json!({
                "tenant_id":"tenant-a", "acl":"private", "replay_hash":screen_hash,
            })),
        )
        .await;
        assert_eq!(revoked_screen.status(), axum::http::StatusCode::GONE);
        let revoked_review = handle_decision_shadow_screen_review_status(
            State(state),
            admin_headers,
            body(review_status()),
        )
        .await;
        assert_eq!(revoked_review.status(), axum::http::StatusCode::GONE);
    }
}

#[cfg(test)]
mod decision_sla_shadow_api_tests {
    use super::*;
    use crate::decision_store::c7_synthetic_shadow_harness::seed_sla_dashboard_window;
    use duduclaw_auth::UserRole;
    use duduclaw_memory::causal::EvidenceScope;
    use rusqlite::{Connection, params};

    fn body(value: serde_json::Value) -> Bytes {
        Bytes::from(serde_json::to_vec(&value).unwrap())
    }

    async fn response_json(response: axum::response::Response) -> serde_json::Value {
        serde_json::from_slice(
            &axum::body::to_bytes(response.into_body(), 256 * 1024)
                .await
                .unwrap(),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn shadow_sla_http_binds_prospective_sources_and_review_states() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();
        let user_db = Arc::new(UserDb::new(&home.join("users.db")).unwrap());
        let admin = user_db
            .create_user(
                "sla-shadow-admin@example.test",
                "Admin",
                "test-password",
                UserRole::Admin,
            )
            .unwrap();
        let employee = user_db
            .create_user(
                "sla-shadow-employee@example.test",
                "Employee",
                "test-password",
                UserRole::Employee,
            )
            .unwrap();
        let jwt_config = Arc::new(JwtConfig::new(b"sla-shadow-api-test-secret-32-bytes!!"));
        let admin_token = jwt_config.issue_access_token(&admin, &[]).unwrap();
        let employee_token = jwt_config.issue_access_token(&employee, &[]).unwrap();
        let (tx, _) = broadcast::channel(4);
        let (event_tx, _) = broadcast::channel(4);
        let state = Arc::new(AppState {
            auth: AuthManager::new(None),
            handler: MethodHandler::new(home.clone()).await,
            tx,
            event_tx,
            user_db,
            jwt_config,
            otp_delivery: Arc::new(crate::otp_delivery::ConfigOtpDeliverer::new(
                home.clone(),
                reqwest::Client::new(),
            )),
            home_dir: home.clone(),
        });
        let mut admin_headers = axum::http::HeaderMap::new();
        admin_headers.insert(
            "authorization",
            format!("Bearer {admin_token}").parse().unwrap(),
        );
        admin_headers.insert("origin", "http://localhost:18789".parse().unwrap());
        let mut employee_headers = admin_headers.clone();
        employee_headers.insert(
            "authorization",
            format!("Bearer {employee_token}").parse().unwrap(),
        );
        let causal = CausalStore::new(home.join("memory.db"));
        let store = DecisionStore::with_causal_store(home.join("decisions.db"), causal.clone());
        let scope = DecisionScope {
            tenant_id: "tenant-a".into(),
            acl: "private".into(),
        };
        let evidence_scope = EvidenceScope {
            tenant_id: scope.tenant_id.clone(),
            acl: scope.acl.clone(),
        };
        // Test-only historical time seam: production request bodies cannot
        // specify these timestamps or backdate an SLA commitment.
        let retained_until = chrono::Utc::now() + chrono::Duration::days(30);
        let retention_utc = retained_until.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let seed = seed_sla_dashboard_window(
            &store,
            &causal,
            &scope,
            &evidence_scope,
            "sla-dashboard-policy",
            retained_until.timestamp(),
        );
        let mut seen = Vec::new();

        let denied = handle_decision_shadow_sla_forecast_load(
            State(state.clone()),
            employee_headers,
            body(serde_json::json!({
                "tenant_id":"tenant-a", "acl":"private", "sla_id":seed.last_sla_forecast_id,
            })),
        )
        .await;
        assert_eq!(denied.status(), axum::http::StatusCode::FORBIDDEN);
        let unknown = handle_decision_shadow_sla_forecast_load(
            State(state.clone()),
            admin_headers.clone(),
            body(serde_json::json!({
                "tenant_id":"tenant-a", "acl":"private", "sla_id":seed.last_sla_forecast_id,
                "ticket_secret":"secret-ticket-id",
            })),
        )
        .await;
        assert_eq!(unknown.status(), axum::http::StatusCode::BAD_REQUEST);
        let unknown = response_json(unknown).await;
        assert!(!unknown.to_string().contains("secret-ticket-id"));
        seen.push(unknown);

        let loaded = handle_decision_shadow_sla_forecast_load(
            State(state.clone()),
            admin_headers.clone(),
            body(serde_json::json!({
                "tenant_id":"tenant-a", "acl":"private", "sla_id":seed.last_sla_forecast_id,
            })),
        )
        .await;
        assert_eq!(loaded.status(), axum::http::StatusCode::OK);
        let loaded = response_json(loaded).await;
        assert!(loaded["sla_forecast"]["opening"]["opening_backlog"].is_string());
        assert!(loaded["sla_forecast"]["opening"]["prior_resolved_ticket_count"].is_number());
        assert!(loaded["sla_forecast"]["prediction"]["predicted_resolved_within_sla"].is_number());
        assert!(loaded["sla_forecast"]["baselines"]["seven_day_mean"].is_number());
        assert!(loaded["sla_forecast"].get("inputs").is_none());
        let opening_sha256 = loaded["sla_forecast"]["opening_sha256"]
            .as_str()
            .unwrap()
            .to_owned();
        seen.push(loaded.clone());

        let opening_sources = || -> i64 {
            Connection::open(causal.path())
                .unwrap()
                .query_row(
                    "SELECT COUNT(*) FROM causal_artifacts
                     WHERE tenant_id=?1 AND acl=?2 AND kind='shadow_sla_opening_export'",
                    params![scope.tenant_id, scope.acl],
                    |row| row.get(0),
                )
                .unwrap()
        };
        let sources_before_retry = opening_sources();
        let forecast_retry = serde_json::json!({
            "tenant_id":"tenant-a", "acl":"private", "sla_id":seed.last_sla_forecast_id,
            "forecast_id":seed.last_forecast_id, "model_version":seed.model_version,
            "opening_source_json":seed.last_opening_json, "retention_until_utc":retention_utc,
        });
        // The opening export carries every open ticket identity, so this body
        // is far past the 16 KiB decision-model request cap.
        let retry_body = body(forecast_retry.clone());
        assert!(retry_body.len() > 16 * 1024);
        assert!(retry_body.len() < MAX_SHADOW_SLA_SOURCE_BODY_BYTES);
        let retried = handle_decision_shadow_sla_forecast_create(
            State(state.clone()),
            admin_headers.clone(),
            retry_body,
        )
        .await;
        assert_eq!(retried.status(), axum::http::StatusCode::OK);
        let retried = response_json(retried).await;
        assert_eq!(retried["sla_forecast"], loaded["sla_forecast"]);
        seen.push(retried);

        let mut oversized = forecast_retry.clone();
        oversized["opening_source_json"] =
            serde_json::json!("x".repeat(MAX_SHADOW_SLA_SOURCE_BODY_BYTES));
        let too_large = handle_decision_shadow_sla_forecast_create(
            State(state.clone()),
            admin_headers.clone(),
            body(oversized),
        )
        .await;
        assert_eq!(
            too_large.status(),
            axum::http::StatusCode::PAYLOAD_TOO_LARGE
        );
        let mut past_source_cap = forecast_retry.clone();
        past_source_cap["opening_source_json"] =
            serde_json::json!("x".repeat(2 * 1024 * 1024 + 1));
        let refused = handle_decision_shadow_sla_forecast_create(
            State(state.clone()),
            admin_headers.clone(),
            body(past_source_cap),
        )
        .await;
        assert_eq!(refused.status(), axum::http::StatusCode::BAD_REQUEST);

        let mut changed_retention = forecast_retry.clone();
        changed_retention["retention_until_utc"] = serde_json::json!(
            (retained_until + chrono::Duration::seconds(1))
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
        );
        let conflicted = handle_decision_shadow_sla_forecast_create(
            State(state.clone()),
            admin_headers.clone(),
            body(changed_retention),
        )
        .await;
        assert_eq!(conflicted.status(), axum::http::StatusCode::CONFLICT);
        assert_eq!(opening_sources(), sources_before_retry);

        let score_create = serde_json::json!({
            "tenant_id":"tenant-a", "acl":"private", "score_id":"sla-score-dashboard",
            "sla_forecast_id":seed.last_sla_forecast_id,
            "aggregate_score_id":seed.last_aggregate_score_id,
            "observation_source_json":seed.last_observation_json,
            "retention_until_utc":retention_utc,
        });
        let scored = handle_decision_shadow_sla_score_create(
            State(state.clone()),
            admin_headers.clone(),
            body(score_create.clone()),
        )
        .await;
        assert_eq!(scored.status(), axum::http::StatusCode::OK);
        let scored = response_json(scored).await;
        assert!(scored["sla_score"]["abs_error"].is_string());
        assert!(scored["sla_score"]["observed_resolved_within_sla"].is_string());
        assert_eq!(scored["sla_score"]["correction_revision"], false);
        assert!(scored["sla_score"].get("observed").is_none());
        seen.push(scored.clone());
        let reused = handle_decision_shadow_sla_score_create(
            State(state.clone()),
            admin_headers.clone(),
            body(score_create),
        )
        .await;
        assert_eq!(reused.status(), axum::http::StatusCode::OK);
        assert_eq!(response_json(reused).await, scored);

        let loaded_score = response_json(
            handle_decision_shadow_sla_score_load(
                State(state.clone()),
                admin_headers.clone(),
                body(serde_json::json!({
                    "tenant_id":"tenant-a", "acl":"private", "score_id":"sla-score-dashboard",
                })),
            )
            .await,
        )
        .await;
        assert_eq!(loaded_score, scored);
        seen.push(loaded_score);
        let current_score = response_json(
            handle_decision_shadow_sla_score_load_current(
                State(state.clone()),
                admin_headers.clone(),
                body(serde_json::json!({
                    "tenant_id":"tenant-a", "acl":"private",
                    "sla_forecast_id":seed.last_sla_forecast_id,
                })),
            )
            .await,
        )
        .await;
        assert_eq!(current_score, scored);
        seen.push(current_score);

        let assessment = response_json(
            handle_decision_shadow_sla_policy_assess(
                State(state.clone()),
                admin_headers.clone(),
                body(serde_json::json!({
                    "tenant_id":"tenant-a", "acl":"private", "policy_id":seed.policy_id,
                })),
            )
            .await,
        )
        .await;
        assert_eq!(assessment["assessment"]["complete"], true);
        assert_eq!(assessment["assessment"]["scored_days"], 21);
        assert_eq!(assessment["assessment"]["day_status_counts"]["scored"], 21);
        assert!(assessment["assessment"]["error_sums"]["model"].is_string());
        assert!(assessment["assessment"].get("days").is_none());
        assert!(
            assessment["assessment"]["fixed_prefix_interval"]
                .get("points")
                .is_none()
        );
        seen.push(assessment);

        let criteria = || {
            serde_json::json!({
                "tenant_id":"tenant-a", "acl":"private", "policy_id":seed.policy_id,
                "min_complete_days":21, "min_fixed_coverage_bps":8000,
            })
        };
        let preview = response_json(
            handle_decision_sla_shadow_screen_evaluate(
                State(state.clone()),
                admin_headers.clone(),
                body(criteria()),
            )
            .await,
        )
        .await;
        assert_eq!(preview["screen"]["record_sha256"], serde_json::Value::Null);
        assert_eq!(
            preview["screen"]["report"]["eligible_for_human_review"],
            true
        );
        seen.push(preview);
        let saved_screen = handle_decision_sla_shadow_screen_save(
            State(state.clone()),
            admin_headers.clone(),
            body(criteria()),
        )
        .await;
        assert_eq!(saved_screen.status(), axum::http::StatusCode::OK);
        let saved_screen = response_json(saved_screen).await;
        assert_eq!(
            saved_screen["screen"]["report"]["eligible_for_human_review"],
            true
        );
        let screen_hash = saved_screen["screen"]["replay_hash"]
            .as_str()
            .unwrap()
            .to_owned();
        seen.push(saved_screen.clone());
        let loaded_screen = response_json(
            handle_decision_sla_shadow_screen_load(
                State(state.clone()),
                admin_headers.clone(),
                body(serde_json::json!({
                    "tenant_id":"tenant-a", "acl":"private", "replay_hash":screen_hash,
                })),
            )
            .await,
        )
        .await;
        assert_eq!(loaded_screen, saved_screen);
        seen.push(loaded_screen);

        let requested = handle_decision_sla_shadow_screen_review_request(
            State(state.clone()),
            admin_headers.clone(),
            body(serde_json::json!({
                "tenant_id":"tenant-a", "acl":"private", "replay_hash":screen_hash,
                "summary":"Inspect synthetic ticket-SLA shadow history", "ttl_seconds":3600,
            })),
        )
        .await;
        assert_eq!(requested.status(), axum::http::StatusCode::OK);
        let requested = response_json(requested).await;
        assert_eq!(requested["review"]["status"], "pending");
        let approval_id = requested["review"]["link"]["approval_id"]
            .as_str()
            .unwrap()
            .to_owned();
        seen.push(requested);
        let review_status = || {
            serde_json::json!({
                "tenant_id":"tenant-a", "acl":"private", "replay_hash":screen_hash,
                "approval_id":approval_id,
            })
        };
        let wrong_scope = handle_decision_sla_shadow_screen_review_status(
            State(state.clone()),
            admin_headers.clone(),
            body(serde_json::json!({
                "tenant_id":"other", "acl":"private", "replay_hash":screen_hash,
                "approval_id":approval_id,
            })),
        )
        .await;
        assert_eq!(wrong_scope.status(), axum::http::StatusCode::NOT_FOUND);
        let broker = crate::approval::ApprovalBroker::open(&home).unwrap();
        broker
            .decide(
                &crate::approval::ApprovalId::from(approval_id.clone()),
                true,
                "human",
            )
            .await
            .unwrap();
        let approved = response_json(
            handle_decision_sla_shadow_screen_review_status(
                State(state.clone()),
                admin_headers.clone(),
                body(review_status()),
            )
            .await,
        )
        .await;
        assert_eq!(approved["review"]["status"], "approved");
        seen.push(approved);

        // No response may carry ticket identities or raw source rows.
        let rendered = serde_json::to_string(&seen).unwrap();
        for forbidden in [
            "ticket_id",
            "opening_tickets",
            "prior_resolved_tickets",
            "\"tickets\"",
            "opening-0",
            "prior-0-0",
            "arrival-20-0",
        ] {
            assert!(
                !rendered.contains(forbidden),
                "SLA dashboard response leaked {forbidden}"
            );
        }

        store.revoke_source_version(&scope, &opening_sha256).unwrap();
        let revoked_forecast = handle_decision_shadow_sla_forecast_load(
            State(state.clone()),
            admin_headers.clone(),
            body(serde_json::json!({
                "tenant_id":"tenant-a", "acl":"private", "sla_id":seed.last_sla_forecast_id,
            })),
        )
        .await;
        assert_eq!(revoked_forecast.status(), axum::http::StatusCode::GONE);
        let revoked_screen = handle_decision_sla_shadow_screen_load(
            State(state.clone()),
            admin_headers.clone(),
            body(serde_json::json!({
                "tenant_id":"tenant-a", "acl":"private", "replay_hash":screen_hash,
            })),
        )
        .await;
        assert_eq!(revoked_screen.status(), axum::http::StatusCode::GONE);
        let revoked_review = handle_decision_sla_shadow_screen_review_status(
            State(state),
            admin_headers,
            body(review_status()),
        )
        .await;
        assert_eq!(revoked_review.status(), axum::http::StatusCode::GONE);
    }
}

#[cfg(test)]
mod decision_model_candidate_api_tests {
    use super::*;
    use crate::decision_calibration::ObservedSupportDay;
    use crate::decision_ingest::{
        DailyStaffing, SupportPilotExport, TicketEvent, derive_ticket_sla_labels,
    };
    use crate::decision_sim::{DecisionSnapshot, QueueModel, StaffingScenario};
    use crate::decision_store::ObservedOutcomeExport;
    use duduclaw_auth::UserRole;
    use rusqlite::{Connection, params};
    use sha2::{Digest, Sha256};
    use std::collections::VecDeque;

    fn body(value: serde_json::Value) -> Bytes {
        Bytes::from(serde_json::to_vec(&value).unwrap())
    }

    async fn json_response(response: axum::response::Response) -> serde_json::Value {
        serde_json::from_slice(
            &axum::body::to_bytes(response.into_body(), 64 * 1024)
                .await
                .unwrap(),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn model_candidate_http_requires_scope_timing_and_active_sources() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();
        let user_db = Arc::new(UserDb::new(&home.join("users.db")).unwrap());
        let admin = user_db
            .create_user(
                "candidate-admin@example.test",
                "Admin",
                "test-password",
                UserRole::Admin,
            )
            .unwrap();
        let employee = user_db
            .create_user(
                "candidate-employee@example.test",
                "Employee",
                "test-password",
                UserRole::Employee,
            )
            .unwrap();
        let jwt_config = Arc::new(JwtConfig::new(b"candidate-api-test-secret-32-bytes!!!"));
        let admin_token = jwt_config.issue_access_token(&admin, &[]).unwrap();
        let employee_token = jwt_config.issue_access_token(&employee, &[]).unwrap();
        let (tx, _) = broadcast::channel(4);
        let (event_tx, _) = broadcast::channel(4);
        let state = Arc::new(AppState {
            auth: AuthManager::new(None),
            handler: MethodHandler::new(home.clone()).await,
            tx,
            event_tx,
            user_db,
            jwt_config,
            otp_delivery: Arc::new(crate::otp_delivery::ConfigOtpDeliverer::new(
                home.clone(),
                reqwest::Client::new(),
            )),
            home_dir: home.clone(),
        });
        let mut admin_headers = axum::http::HeaderMap::new();
        admin_headers.insert(
            "authorization",
            format!("Bearer {admin_token}").parse().unwrap(),
        );
        admin_headers.insert("origin", "http://localhost:18789".parse().unwrap());
        let mut employee_headers = admin_headers.clone();
        employee_headers.insert(
            "authorization",
            format!("Bearer {employee_token}").parse().unwrap(),
        );
        let store = DecisionStore::with_causal_store(
            home.join("decisions.db"),
            CausalStore::new(home.join("memory.db")),
        );
        let scope = DecisionScope {
            tenant_id: "tenant-a".into(),
            acl: "private".into(),
        };
        let start = chrono::Utc::now()
            .date_naive()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc()
            - chrono::Duration::days(30);
        let training_source = format!("{:x}", Sha256::digest(b"candidate-training-source"));
        let target_source = format!("{:x}", Sha256::digest(b"candidate-target-source"));
        let timestamp = |days| {
            (start + chrono::Duration::days(days))
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
        };
        let snapshot = DecisionSnapshot {
            id: "candidate-training".into(),
            queue_id: Some("support".into()),
            data_cutoff_utc: timestamp(0),
            source_version_hashes: vec![training_source.clone()],
            seed: 1,
            arrivals_by_day: vec![5; 7],
            initial_backlog: vec![],
        };
        let model = QueueModel {
            version: "parent-model".into(),
            service_capacity_per_agent_day: 2,
            sla_days: 2,
            staff_cost_cents_per_agent_day: 100,
        };
        let scenario = StaffingScenario {
            id: "staffing".into(),
            agents_by_day: vec![1; 7],
            fixed_extra_capacity_by_day: vec![0; 7],
        };
        store.put_snapshot(&scope, &snapshot).unwrap();
        store.put_model(&scope, &model).unwrap();
        store.put_scenario(&scope, &scenario).unwrap();
        let original_run = store
            .put_daily_run(&scope, &snapshot.id, &model.version, &scenario.id)
            .unwrap();
        let conn = Connection::open(store.path()).unwrap();
        conn.execute(
            "UPDATE decision_inputs SET created_at=?1 WHERE tenant_id=?2 AND acl=?3 AND kind='daily_run' AND input_id=?4",
            params![start.timestamp() - 86_400, scope.tenant_id, scope.acl, original_run.replay_hash],
        ).unwrap();
        let observed_days = (0..7)
            .map(|day| ObservedSupportDay {
                arrivals: 5,
                backlog_start: day * 2,
                resolved: 3,
                backlog_end: (day + 1) * 2,
                agents: 1,
                fixed_extra_capacity: 0,
            })
            .collect::<Vec<_>>();
        let observed = ObservedOutcomeExport {
            sla_days: None,
            resolved_within_sla_by_day: None,
            queue_id: Some("support".into()),
            window_start_utc: timestamp(0),
            observed_through_utc: timestamp(7),
            observed_days: observed_days.clone(),
        };
        store
            .record_observed_outcome(
                &scope,
                "training-observed",
                &snapshot.id,
                &model.version,
                &scenario.id,
                &original_run.replay_hash,
                "recorder",
                &serde_json::to_vec(&observed).unwrap(),
            )
            .unwrap();
        let fit_request = || {
            serde_json::json!({
                "tenant_id":"tenant-a", "acl":"private", "fit_id":"capacity-fit",
                "outcome_id":"training-observed", "min_saturated_days":3,
                "training_days":4,
            })
        };
        let fit_denied = handle_decision_outcome_fit_create(
            State(state.clone()),
            employee_headers.clone(),
            body(fit_request()),
        )
        .await;
        assert_eq!(fit_denied.status(), axum::http::StatusCode::FORBIDDEN);
        let unknown_fit = handle_decision_outcome_fit_create(
            State(state.clone()),
            admin_headers.clone(),
            body(serde_json::json!({
                "tenant_id":"tenant-a", "acl":"private", "fit_id":"capacity-fit",
                "outcome_id":"training-observed", "min_saturated_days":3,
                "training_days":4, "ticket_secret":"raw-ticket-secret",
            })),
        )
        .await;
        assert_eq!(unknown_fit.status(), axum::http::StatusCode::BAD_REQUEST);
        assert!(
            !json_response(unknown_fit)
                .await
                .to_string()
                .contains("raw-ticket-secret")
        );
        let short_fit = handle_decision_outcome_fit_create(
            State(state.clone()),
            admin_headers.clone(),
            body(serde_json::json!({
                "tenant_id":"tenant-a", "acl":"private", "fit_id":"short-fit",
                "outcome_id":"training-observed", "min_saturated_days":3,
                "training_days":5,
            })),
        )
        .await;
        assert_eq!(short_fit.status(), axum::http::StatusCode::BAD_REQUEST);
        let fit_response = handle_decision_outcome_fit_create(
            State(state.clone()),
            admin_headers.clone(),
            body(fit_request()),
        )
        .await;
        assert_eq!(fit_response.status(), axum::http::StatusCode::OK);
        let fit_response = json_response(fit_response).await;
        assert_eq!(fit_response["fit"]["fit"]["service_per_agent_day"], 3);
        assert!(fit_response["fit"]["holdout"]["candidate_abs_error_sum"].is_string());
        assert!(fit_response["fit"].get("observed_days").is_none());
        let fit_loaded = json_response(
            handle_decision_outcome_fit_load(
                State(state.clone()),
                admin_headers.clone(),
                body(serde_json::json!({
                    "tenant_id":"tenant-a", "acl":"private", "fit_id":"capacity-fit",
                })),
            )
            .await,
        )
        .await;
        assert_eq!(fit_loaded, fit_response);
        let fit_wrong_scope = handle_decision_outcome_fit_load(
            State(state.clone()),
            admin_headers.clone(),
            body(serde_json::json!({
                "tenant_id":"other", "acl":"private", "fit_id":"capacity-fit",
            })),
        )
        .await;
        assert_eq!(fit_wrong_scope.status(), axum::http::StatusCode::NOT_FOUND);
        let failed_screen = handle_decision_outcome_screen_save(
            State(state.clone()),
            admin_headers.clone(),
            body(serde_json::json!({
                "tenant_id":"tenant-a", "acl":"private", "fit_id":"capacity-fit",
                "min_saturated_days":5, "min_holdout_days":4,
            })),
        )
        .await;
        assert_eq!(failed_screen.status(), axum::http::StatusCode::OK);
        let failed_screen = json_response(failed_screen).await;
        assert_eq!(
            failed_screen["screen"]["report"]["eligible_for_human_review"],
            false
        );
        let refused_review = handle_decision_outcome_screen_review_request(
            State(state.clone()),
            admin_headers.clone(),
            body(serde_json::json!({
                "tenant_id":"tenant-a", "acl":"private",
                "replay_hash":failed_screen["screen"]["replay_hash"],
                "summary":"Inspect failed screen", "ttl_seconds":3600,
            })),
        )
        .await;
        assert_eq!(refused_review.status(), axum::http::StatusCode::CONFLICT);
        let screen_response = handle_decision_outcome_screen_save(
            State(state.clone()),
            admin_headers.clone(),
            body(serde_json::json!({
                "tenant_id":"tenant-a", "acl":"private", "fit_id":"capacity-fit",
                "min_saturated_days":3, "min_holdout_days":3,
            })),
        )
        .await;
        assert_eq!(screen_response.status(), axum::http::StatusCode::OK);
        let screen_response = json_response(screen_response).await;
        let screen = &screen_response["screen"];
        assert_eq!(screen["report"]["eligible_for_human_review"], true);
        assert!(screen["report"]["holdout"]["candidate_abs_error_sum"].is_string());
        let screen_hash = screen["replay_hash"].as_str().unwrap().to_owned();
        let screen_loaded = json_response(
            handle_decision_outcome_screen_load(
                State(state.clone()),
                admin_headers.clone(),
                body(serde_json::json!({
                    "tenant_id":"tenant-a", "acl":"private", "replay_hash":screen_hash,
                })),
            )
            .await,
        )
        .await;
        assert_eq!(screen_loaded, screen_response);
        let review_request = || {
            serde_json::json!({
                "tenant_id":"tenant-a", "acl":"private", "replay_hash":screen_hash,
                "summary":"Inspect synthetic capacity fit", "ttl_seconds":3600,
            })
        };
        let requested_review = handle_decision_outcome_screen_review_request(
            State(state.clone()),
            admin_headers.clone(),
            body(review_request()),
        )
        .await;
        assert_eq!(requested_review.status(), axum::http::StatusCode::OK);
        let requested_review = json_response(requested_review).await;
        assert_eq!(requested_review["review"]["status"], "pending");
        let approval_id = requested_review["review"]["link"]["approval_id"]
            .as_str()
            .unwrap()
            .to_owned();
        let review_status_request = || {
            serde_json::json!({
                "tenant_id":"tenant-a", "acl":"private", "replay_hash":screen_hash,
                "approval_id":approval_id,
            })
        };
        let wrong_review_scope = handle_decision_outcome_screen_review_status(
            State(state.clone()),
            admin_headers.clone(),
            body(serde_json::json!({
                "tenant_id":"other", "acl":"private", "replay_hash":screen_hash,
                "approval_id":approval_id,
            })),
        )
        .await;
        assert_eq!(
            wrong_review_scope.status(),
            axum::http::StatusCode::NOT_FOUND
        );
        let wrong_review_hash = handle_decision_outcome_screen_review_status(
            State(state.clone()),
            admin_headers.clone(),
            body(serde_json::json!({
                "tenant_id":"tenant-a", "acl":"private", "replay_hash":"0".repeat(64),
                "approval_id":approval_id,
            })),
        )
        .await;
        assert_eq!(wrong_review_hash.status(), axum::http::StatusCode::CONFLICT);
        let broker = crate::approval::ApprovalBroker::open(&home).unwrap();
        broker
            .decide(
                &crate::approval::ApprovalId::from(approval_id.clone()),
                true,
                "human-reviewer",
            )
            .await
            .unwrap();
        let approved_review = handle_decision_outcome_screen_review_status(
            State(state.clone()),
            admin_headers.clone(),
            body(review_status_request()),
        )
        .await;
        assert_eq!(approved_review.status(), axum::http::StatusCode::OK);
        let approved_review = json_response(approved_review).await;
        assert_eq!(approved_review["review"]["status"], "approved");
        assert_eq!(approved_review["review"]["decided_by"], "human-reviewer");
        let denied_link = json_response(
            handle_decision_outcome_screen_review_request(
                State(state.clone()),
                admin_headers.clone(),
                body(review_request()),
            )
            .await,
        )
        .await;
        let denied_id = denied_link["review"]["link"]["approval_id"]
            .as_str()
            .unwrap()
            .to_owned();
        broker
            .decide(
                &crate::approval::ApprovalId::from(denied_id.clone()),
                false,
                "human-reviewer",
            )
            .await
            .unwrap();
        let denied_status = json_response(
            handle_decision_outcome_screen_review_status(
                State(state.clone()),
                admin_headers.clone(),
                body(serde_json::json!({
                    "tenant_id":"tenant-a", "acl":"private", "replay_hash":screen_hash,
                    "approval_id":denied_id,
                })),
            )
            .await,
        )
        .await;
        assert_eq!(denied_status["review"]["status"], "denied");
        let expiring_link = json_response(
            handle_decision_outcome_screen_review_request(
                State(state.clone()),
                admin_headers.clone(),
                body(serde_json::json!({
                    "tenant_id":"tenant-a", "acl":"private", "replay_hash":screen_hash,
                    "summary":"Expire this inspection", "ttl_seconds":1,
                })),
            )
            .await,
        )
        .await;
        let expiring_id = expiring_link["review"]["link"]["approval_id"]
            .as_str()
            .unwrap()
            .to_owned();
        tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
        let expired_status = json_response(
            handle_decision_outcome_screen_review_status(
                State(state.clone()),
                admin_headers.clone(),
                body(serde_json::json!({
                    "tenant_id":"tenant-a", "acl":"private", "replay_hash":screen_hash,
                    "approval_id":expiring_id,
                })),
            )
            .await,
        )
        .await;
        assert_eq!(expired_status["review"]["status"], "expired");
        let mut late_snapshot = snapshot.clone();
        late_snapshot.id = "late-training".into();
        store.put_snapshot(&scope, &late_snapshot).unwrap();
        let late_run = store
            .put_daily_run(&scope, &late_snapshot.id, &model.version, &scenario.id)
            .unwrap();
        store
            .record_observed_outcome(
                &scope,
                "late-training-observed",
                &late_snapshot.id,
                &model.version,
                &scenario.id,
                &late_run.replay_hash,
                "recorder",
                &serde_json::to_vec(&observed).unwrap(),
            )
            .unwrap();
        let late_fit = handle_decision_outcome_fit_create(
            State(state.clone()),
            admin_headers.clone(),
            body(serde_json::json!({
                "tenant_id":"tenant-a", "acl":"private", "fit_id":"late-capacity-fit",
                "outcome_id":"late-training-observed", "min_saturated_days":3,
                "training_days":4,
            })),
        )
        .await;
        assert_eq!(late_fit.status(), axum::http::StatusCode::OK);
        let late_screen = json_response(
            handle_decision_outcome_screen_save(
                State(state.clone()),
                admin_headers.clone(),
                body(serde_json::json!({
                    "tenant_id":"tenant-a", "acl":"private", "fit_id":"late-capacity-fit",
                    "min_saturated_days":3, "min_holdout_days":3,
                })),
            )
            .await,
        )
        .await;
        assert_eq!(
            late_screen["screen"]["report"]["eligible_for_human_review"],
            false
        );
        assert!(
            late_screen["screen"]["report"]["failed_checks"]
                .as_array()
                .unwrap()
                .iter()
                .any(|value| value == "run_not_committed_before_observation_window")
        );
        let late_review = handle_decision_outcome_screen_review_request(
            State(state.clone()),
            admin_headers.clone(),
            body(serde_json::json!({
                "tenant_id":"tenant-a", "acl":"private",
                "replay_hash":late_screen["screen"]["replay_hash"],
                "summary":"Late observation", "ttl_seconds":3600,
            })),
        )
        .await;
        assert_eq!(late_review.status(), axum::http::StatusCode::CONFLICT);
        let create = || {
            serde_json::json!({
                "tenant_id":"tenant-a", "acl":"private", "candidate_id":"capacity-v2",
                "screen_replay_hash":screen_hash,
            })
        };
        let unauthorized = handle_decision_model_candidate_create(
            State(state.clone()),
            employee_headers,
            body(create()),
        )
        .await;
        assert_eq!(unauthorized.status(), axum::http::StatusCode::FORBIDDEN);
        let missing = handle_decision_model_candidate_create(
            State(state.clone()),
            admin_headers.clone(),
            body(serde_json::json!({
                "tenant_id":"tenant-a", "acl":"private", "candidate_id":"missing-screen",
                "screen_replay_hash":"0".repeat(64),
            })),
        )
        .await;
        assert_eq!(missing.status(), axum::http::StatusCode::NOT_FOUND);
        let unknown_field = handle_decision_model_candidate_create(
            State(state.clone()),
            admin_headers.clone(),
            body(serde_json::json!({
                "tenant_id":"tenant-a", "acl":"private", "candidate_id":"capacity-v2",
                "screen_replay_hash":screen_hash, "ticket_secret":"raw-ticket-secret",
            })),
        )
        .await;
        assert_eq!(unknown_field.status(), axum::http::StatusCode::BAD_REQUEST);
        assert!(
            !json_response(unknown_field)
                .await
                .to_string()
                .contains("raw-ticket-secret")
        );
        let created = handle_decision_model_candidate_create(
            State(state.clone()),
            admin_headers.clone(),
            body(create()),
        )
        .await;
        assert_eq!(created.status(), axum::http::StatusCode::OK);
        let created = json_response(created).await;
        assert_eq!(
            created["candidate"]["model"]["service_capacity_per_agent_day"],
            3
        );
        // The lineage list is sorted, and the sibling observation digest is
        // derived from today's date, so its position relative to the fixed
        // training digest flips with the calendar. Assert membership, not
        // index: indexing here made this test fail on roughly half of all days.
        assert!(
            created["candidate"]["source_version_hashes"]
                .as_array()
                .unwrap()
                .iter()
                .any(|value| value == &serde_json::Value::String(training_source.clone()))
        );
        assert!(created["candidate"]["record_sha256"].as_str().is_some());
        let repeat = json_response(
            handle_decision_model_candidate_create(
                State(state.clone()),
                admin_headers.clone(),
                body(create()),
            )
            .await,
        )
        .await;
        assert_eq!(repeat, created);
        let wrong_scope = handle_decision_model_candidate_load(
            State(state.clone()),
            admin_headers.clone(),
            body(serde_json::json!({
                "tenant_id":"other", "acl":"private", "candidate_id":"capacity-v2",
            })),
        )
        .await;
        assert_eq!(wrong_scope.status(), axum::http::StatusCode::NOT_FOUND);
        let loaded = json_response(
            handle_decision_model_candidate_load(
                State(state.clone()),
                admin_headers.clone(),
                body(serde_json::json!({
                    "tenant_id":"tenant-a", "acl":"private", "candidate_id":"capacity-v2",
                })),
            )
            .await,
        )
        .await;
        assert_eq!(loaded, created);
        assert!(matches!(
            store.get::<QueueModel>(&scope, "model", "capacity-v2"),
            Err(DecisionStoreError::NotFound)
        ));

        let mut target = snapshot.clone();
        target.id = "later-snapshot".into();
        target.data_cutoff_utc = timestamp(10);
        target.source_version_hashes = vec![target_source.clone()];
        store.put_snapshot(&scope, &target).unwrap();
        let compare = || {
            serde_json::json!({
                "tenant_id":"tenant-a", "acl":"private", "candidate_id":"capacity-v2",
                "target_snapshot_id":target.id, "scenario_id":scenario.id,
            })
        };
        let compared = handle_decision_model_candidate_compare(
            State(state.clone()),
            admin_headers.clone(),
            body(compare()),
        )
        .await;
        assert_eq!(compared.status(), axum::http::StatusCode::OK);
        let compared = json_response(compared).await;
        assert!(compared["run"]["replay_hash"].as_str().is_some());
        // KPIs and deltas travel as decimal strings: a JSON number cannot
        // carry them exactly once they pass 2^53, and the dashboard re-derives
        // the deltas from them with BigInt.
        assert!(
            compared["run"]["parent"]["final_backlog"]
                .as_str()
                .is_some_and(|value| value.parse::<u64>().is_ok())
        );
        assert!(compared["run"]["parent"].get("days").is_none());
        assert!(
            compared["run"]["final_backlog_delta"]
                .as_str()
                .unwrap()
                .parse::<i64>()
                .unwrap()
                < 0
        );
        let replay_hash = compared["run"]["replay_hash"].as_str().unwrap().to_owned();
        let loaded_run = json_response(
            handle_decision_model_candidate_load_run(
                State(state.clone()),
                admin_headers.clone(),
                body(serde_json::json!({
                    "tenant_id":"tenant-a", "acl":"private", "replay_hash":replay_hash,
                })),
            )
            .await,
        )
        .await;
        assert_eq!(loaded_run, compared);
        let target_run = store
            .put_daily_run(&scope, &target.id, &model.version, &scenario.id)
            .unwrap();
        conn.execute(
            "UPDATE decision_inputs SET created_at=?1 WHERE tenant_id=?2 AND acl=?3 AND kind='daily_run' AND input_id=?4",
            params![(start + chrono::Duration::days(9)).timestamp(), scope.tenant_id, scope.acl, target_run.replay_hash],
        ).unwrap();
        let mut tickets = Vec::<TicketEvent>::new();
        let mut staffing = Vec::<DailyStaffing>::new();
        let mut pending_tickets = VecDeque::<usize>::new();
        for day in 0..7 {
            let day_utc = timestamp(10 + day);
            staffing.push(DailyStaffing {
                queue_id: Some("support".into()),
                day_utc: day_utc.clone(),
                agents: 1,
                fixed_extra_capacity: 0,
            });
            for ticket in 0..5 {
                pending_tickets.push_back(tickets.len());
                tickets.push(TicketEvent {
                    queue_id: Some("support".into()),
                    ticket_id: format!("secret-ticket-{day}-{ticket}"),
                    created_at_utc: day_utc.clone(),
                    resolved_at_utc: None,
                });
            }
            for _ in 0..3 {
                let index = pending_tickets.pop_front().unwrap();
                tickets[index].resolved_at_utc = Some(
                    (start + chrono::Duration::days(10 + day) + chrono::Duration::hours(12))
                        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                );
            }
        }
        let ticket_rows_sha256 = format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&(&tickets, &staffing)).unwrap())
        );
        let ticket_export = SupportPilotExport {
            snapshot_id: target.id.clone(),
            baseline_scenario_id: scenario.id.clone(),
            window_start_utc: timestamp(10),
            data_cutoff_utc: timestamp(17),
            source_version_hashes: vec![ticket_rows_sha256],
            seed: 1,
            horizon_days: 7,
            tickets,
            staffing,
        };
        let ticket_source_bytes = serde_json::to_vec(&ticket_export).unwrap();
        let ticket_source_sha256 = format!("{:x}", Sha256::digest(&ticket_source_bytes));
        let later = ObservedOutcomeExport {
            sla_days: Some(model.sla_days),
            resolved_within_sla_by_day: Some(
                derive_ticket_sla_labels(&ticket_export, model.sla_days).unwrap(),
            ),
            queue_id: Some("support".into()),
            window_start_utc: timestamp(10),
            observed_through_utc: timestamp(17),
            observed_days,
        };
        store
            .record_observed_outcome_with_ticket_source(
                &scope,
                "later-observed",
                &target.id,
                &model.version,
                &scenario.id,
                &target_run.replay_hash,
                "recorder",
                &serde_json::to_vec(&later).unwrap(),
                &ticket_source_bytes,
                &(chrono::Utc::now() + chrono::Duration::days(30))
                    .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            )
            .unwrap();
        let score = || {
            serde_json::json!({
                "tenant_id":"tenant-a", "acl":"private", "comparison_run_hash":replay_hash,
                "outcome_id":"later-observed",
            })
        };
        let late = handle_decision_model_candidate_score(
            State(state.clone()),
            admin_headers.clone(),
            body(score()),
        )
        .await;
        assert_eq!(late.status(), axum::http::StatusCode::BAD_REQUEST);
        conn.execute(
            "UPDATE decision_inputs SET created_at=?1 WHERE tenant_id=?2 AND acl=?3 AND kind='outcome_model_candidate' AND input_id='capacity-v2'",
            params![(start + chrono::Duration::days(8)).timestamp(), scope.tenant_id, scope.acl],
        ).unwrap();
        conn.execute(
            "UPDATE decision_inputs SET created_at=?1 WHERE tenant_id=?2 AND acl=?3 AND kind='outcome_model_candidate_run' AND input_id=?4",
            params![(start + chrono::Duration::days(9)).timestamp(), scope.tenant_id, scope.acl, replay_hash],
        ).unwrap();
        let scored = handle_decision_model_candidate_score(
            State(state.clone()),
            admin_headers.clone(),
            body(score()),
        )
        .await;
        assert_eq!(scored.status(), axum::http::StatusCode::OK);
        let scored = json_response(scored).await;
        assert_eq!(
            scored["score"]["status"],
            "exploratory_locally_timed_forecast_diagnostic"
        );
        assert_eq!(scored["score"]["candidate"]["backlog_abs_error_sum"], "0");
        assert!(scored["score"]["parent"]["backlog_abs_error_sum"].is_string());
        assert!(scored["score"]["candidate"].get("observed_days").is_none());
        assert_eq!(
            scored["score"]["ticket_source_sha256"],
            ticket_source_sha256
        );
        assert!(scored["score"]["ticket_source_retention_until_utc"].is_string());
        assert!(scored["score"]["candidate"]["resolved_within_sla_abs_error_sum"].is_string());
        assert!(!scored.to_string().contains("secret-ticket-"));
        let score_hash = scored["score"]["replay_hash"].as_str().unwrap().to_owned();
        let loaded_score = json_response(
            handle_decision_model_candidate_load_score(
                State(state.clone()),
                admin_headers.clone(),
                body(serde_json::json!({
                    "tenant_id":"tenant-a", "acl":"private", "replay_hash":score_hash,
                })),
            )
            .await,
        )
        .await;
        assert_eq!(loaded_score, scored);
        let wrong_score = handle_decision_model_candidate_load_score(
            State(state.clone()),
            admin_headers.clone(),
            body(serde_json::json!({
                "tenant_id":"other", "acl":"private", "replay_hash":score_hash,
            })),
        )
        .await;
        assert_eq!(wrong_score.status(), axum::http::StatusCode::NOT_FOUND);

        let mut pending = target.clone();
        pending.id = "pending-upload-snapshot".into();
        store.put_snapshot(&scope, &pending).unwrap();
        conn.execute(
            "INSERT INTO decision_operator_pilot_imports
             (tenant_id,acl,snapshot_id,request_sha256,source_sha256,model_version,baseline_scenario_id,alternative_scenario_id)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            params![scope.tenant_id, scope.acl, pending.id, "0".repeat(64), target_source,
                model.version, scenario.id, "unused-alternative"],
        ).unwrap();
        let pending_compare = handle_decision_model_candidate_compare(
            State(state.clone()),
            admin_headers.clone(),
            body(serde_json::json!({
                "tenant_id":"tenant-a", "acl":"private", "candidate_id":"capacity-v2",
                "target_snapshot_id":pending.id, "scenario_id":scenario.id,
            })),
        )
        .await;
        assert_eq!(pending_compare.status(), axum::http::StatusCode::NOT_FOUND);
        store
            .revoke_source_version(&scope, &target.source_version_hashes[0])
            .unwrap();
        let revoked_run = handle_decision_model_candidate_load_run(
            State(state.clone()),
            admin_headers.clone(),
            body(serde_json::json!({
                "tenant_id":"tenant-a", "acl":"private", "replay_hash":replay_hash,
            })),
        )
        .await;
        assert_eq!(revoked_run.status(), axum::http::StatusCode::GONE);
        let revoked_score = handle_decision_model_candidate_load_score(
            State(state.clone()),
            admin_headers.clone(),
            body(serde_json::json!({
                "tenant_id":"tenant-a", "acl":"private", "replay_hash":score_hash,
            })),
        )
        .await;
        assert_eq!(revoked_score.status(), axum::http::StatusCode::GONE);
        store
            .revoke_source_version(&scope, &training_source)
            .unwrap();
        let revoked_fit = handle_decision_outcome_fit_load(
            State(state.clone()),
            admin_headers.clone(),
            body(serde_json::json!({
                "tenant_id":"tenant-a", "acl":"private", "fit_id":"capacity-fit",
            })),
        )
        .await;
        assert_eq!(revoked_fit.status(), axum::http::StatusCode::GONE);
        let revoked_screen = handle_decision_outcome_screen_load(
            State(state.clone()),
            admin_headers.clone(),
            body(serde_json::json!({
                "tenant_id":"tenant-a", "acl":"private", "replay_hash":screen_hash,
            })),
        )
        .await;
        assert_eq!(revoked_screen.status(), axum::http::StatusCode::GONE);
        let revoked_review = handle_decision_outcome_screen_review_status(
            State(state.clone()),
            admin_headers.clone(),
            body(review_status_request()),
        )
        .await;
        assert_eq!(revoked_review.status(), axum::http::StatusCode::GONE);
        let revoked_candidate = handle_decision_model_candidate_load(
            State(state),
            admin_headers,
            body(serde_json::json!({
                "tenant_id":"tenant-a", "acl":"private", "candidate_id":"capacity-v2",
            })),
        )
        .await;
        assert_eq!(revoked_candidate.status(), axum::http::StatusCode::GONE);
    }
}

#[cfg(test)]
mod causal_curation_api_tests {
    use super::*;
    use duduclaw_auth::UserRole;
    use duduclaw_memory::causal::{ClaimModality, EvidenceStance};
    use duduclaw_memory::causal_model::{ModelDraft, VariableKind};
    use sha2::Digest;

    /// Decision handlers take the raw body and deserialize it only after the
    /// admin check, so these tests hand them the same bytes an HTTP client
    /// would send instead of a pre-deserialized `Json<T>`.
    fn decision_body<T: serde::Serialize>(request: &T) -> Bytes {
        Bytes::from(serde_json::to_vec(request).expect("serialize decision request"))
    }

    /// Query twin of [`decision_body`]: the handler now receives the raw query
    /// string and parses it after authorization.
    fn decision_raw_query<T: serde::Serialize>(query: &T) -> axum::extract::RawQuery {
        let value = serde_json::to_value(query).expect("serialize decision query");
        let fields = value.as_object().expect("decision query object").clone();
        let mut pairs = url::form_urlencoded::Serializer::new(String::new());
        for (key, field) in &fields {
            match field {
                serde_json::Value::Null => {}
                serde_json::Value::String(text) => {
                    pairs.append_pair(key, text);
                }
                other => {
                    pairs.append_pair(key, &other.to_string());
                }
            }
        }
        axum::extract::RawQuery(Some(pairs.finish()))
    }

    /// Regression: these sixteen handlers used axum's `Json<T>` / `Query<T>`
    /// extractors, which reject *before* the handler body runs. An
    /// unauthenticated caller therefore received serde's rejection text
    /// (`unknown field ...`, `invalid type: ... expected ...`) plus its own
    /// input echoed back, which is enough to map an admin-only schema.
    /// Authorization must answer first and the answer must say nothing about
    /// the body.
    #[tokio::test]
    async fn unauthenticated_decision_requests_never_echo_their_schema() {
        async fn assert_denied(name: &str, response: axum::response::Response) {
            let status = response.status();
            assert!(
                matches!(
                    status,
                    axum::http::StatusCode::UNAUTHORIZED | axum::http::StatusCode::FORBIDDEN
                ),
                "{name} answered {status} before authorizing"
            );
            let body = axum::body::to_bytes(response.into_body(), 10_000)
                .await
                .unwrap();
            let text = String::from_utf8_lossy(&body).to_string();
            for leak in [
                "probe_schema_field",
                "probe-secret",
                "unknown field",
                "invalid type",
                "expected",
                "tenant_id",
            ] {
                assert!(!text.contains(leak), "{name} leaked {leak:?} in {text}");
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();
        let user_db = Arc::new(UserDb::new(&home.join("users.db")).unwrap());
        let jwt_config = Arc::new(JwtConfig::new(b"decision-extractor-test-secret-32!!!"));
        let (tx, _) = broadcast::channel(4);
        let (event_tx, _) = broadcast::channel(4);
        let state = Arc::new(AppState {
            auth: AuthManager::new(None),
            handler: MethodHandler::new(home.clone()).await,
            tx,
            event_tx,
            user_db,
            jwt_config,
            otp_delivery: Arc::new(crate::otp_delivery::ConfigOtpDeliverer::new(
                home.clone(),
                reqwest::Client::new(),
            )),
            home_dir: home,
        });
        let mut headers = axum::http::HeaderMap::new();
        headers.insert("origin", "http://localhost:18789".parse().unwrap());
        let body = || {
            Bytes::from_static(b"{\"tenant_id\":1,\"probe_schema_field\":\"probe-secret\"}")
        };
        let raw = || axum::extract::RawQuery(Some("probe_schema_field=probe-secret".into()));

        assert_denied(
            "overview",
            handle_decision_overview(State(state.clone()), headers.clone(), raw()).await,
        )
        .await;
        assert_denied(
            "catalog",
            handle_decision_catalog(State(state.clone()), headers.clone(), raw()).await,
        )
        .await;
        assert_denied(
            "shadow-monitor",
            handle_decision_shadow_monitor(State(state.clone()), headers.clone(), raw()).await,
        )
        .await;
        assert_denied(
            "ticket-sources/scrub",
            handle_decision_ticket_sources_scrub(State(state.clone()), headers.clone(), body())
                .await,
        )
        .await;
        assert_denied(
            "engineering-validation",
            handle_decision_engineering_validation(State(state.clone()), headers.clone(), body())
                .await,
        )
        .await;
        assert_denied(
            "forecast-validation",
            handle_decision_forecast_validation(State(state.clone()), headers.clone(), body())
                .await,
        )
        .await;
        assert_denied(
            "empirical-resampling",
            handle_decision_empirical_resampling(State(state.clone()), headers.clone(), body())
                .await,
        )
        .await;
        assert_denied(
            "synthetic-pilot",
            handle_decision_synthetic_pilot(State(state.clone()), headers.clone(), body()).await,
        )
        .await;
        assert_denied(
            "synthetic-pilot/lifecycle",
            handle_decision_synthetic_pilot_lifecycle(
                State(state.clone()),
                headers.clone(),
                body(),
            )
            .await,
        )
        .await;
        assert_denied(
            "compare",
            handle_decision_compare(State(state.clone()), headers.clone(), body()).await,
        )
        .await;
        assert_denied(
            "event-compare",
            handle_decision_event_compare(State(state.clone()), headers.clone(), body()).await,
        )
        .await;
        assert_denied(
            "replay",
            handle_decision_replay(State(state.clone()), headers.clone(), body()).await,
        )
        .await;
        assert_denied(
            "pilot-review/request",
            handle_decision_pilot_review_request(State(state.clone()), headers.clone(), body())
                .await,
        )
        .await;
        assert_denied(
            "pilot-review/status",
            handle_decision_pilot_review_status(State(state.clone()), headers.clone(), body())
                .await,
        )
        .await;
        assert_denied(
            "policy-sweep",
            handle_decision_policy_sweep(State(state.clone()), headers.clone(), body()).await,
        )
        .await;
        assert_denied(
            "sensitivity",
            handle_decision_sensitivity(State(state.clone()), headers.clone(), body()).await,
        )
        .await;
    }

    #[tokio::test]
    async fn operator_pilot_import_requires_admin_and_never_echoes_ticket_material() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();
        let user_db = Arc::new(UserDb::new(&home.join("users.db")).unwrap());
        let admin = user_db
            .create_user(
                "pilot-admin@example.test",
                "Admin",
                "test-password",
                UserRole::Admin,
            )
            .unwrap();
        let employee = user_db
            .create_user(
                "pilot-employee@example.test",
                "Employee",
                "test-password",
                UserRole::Employee,
            )
            .unwrap();
        let jwt_config = Arc::new(JwtConfig::new(b"pilot-upload-test-secret-32-bytes!!!"));
        let admin_token = jwt_config.issue_access_token(&admin, &[]).unwrap();
        let employee_token = jwt_config.issue_access_token(&employee, &[]).unwrap();
        let (tx, _) = broadcast::channel(4);
        let (event_tx, _) = broadcast::channel(4);
        let state = Arc::new(AppState {
            auth: AuthManager::new(None),
            handler: MethodHandler::new(home.clone()).await,
            tx,
            event_tx,
            user_db,
            jwt_config,
            otp_delivery: Arc::new(crate::otp_delivery::ConfigOtpDeliverer::new(
                home.clone(),
                reqwest::Client::new(),
            )),
            home_dir: home,
        });
        let mut admin_headers = axum::http::HeaderMap::new();
        admin_headers.insert(
            "authorization",
            format!("Bearer {admin_token}").parse().unwrap(),
        );
        admin_headers.insert("origin", "http://localhost:18789".parse().unwrap());
        let mut employee_headers = admin_headers.clone();
        employee_headers.insert(
            "authorization",
            format!("Bearer {employee_token}").parse().unwrap(),
        );
        let mut export = crate::decision_synthetic::synthetic_support_export(88, 21).unwrap();
        export.snapshot_id = "uploaded-api-snapshot-88".into();
        export.baseline_scenario_id = "uploaded-api-baseline-88".into();
        let request = OperatorPilotImportRequest {
            tenant_id: "tenant-a".into(),
            acl: "private".into(),
            expected_queue_id: "synthetic-support-queue".into(),
            source_lineage: "api-upload-lineage".into(),
            retention_until_utc: (chrono::Utc::now() + chrono::Duration::days(30))
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            export,
            model: crate::decision_sim::QueueModel {
                version: "uploaded-api-model-88".into(),
                service_capacity_per_agent_day: 8,
                sla_days: 2,
                staff_cost_cents_per_agent_day: 10_000,
            },
            alternative_scenario: crate::decision_sim::StaffingScenario {
                id: "uploaded-api-alternative-88".into(),
                agents_by_day: vec![3; 21],
                fixed_extra_capacity_by_day: vec![0; 21],
            },
        };
        let body = Bytes::from(serde_json::to_vec(&request).unwrap());
        let denied = handle_decision_import_pilot(
            State(state.clone()),
            employee_headers.clone(),
            body.clone(),
        )
        .await;
        assert_eq!(denied.status(), axum::http::StatusCode::FORBIDDEN);
        let mut wrong_origin = admin_headers.clone();
        wrong_origin.insert("origin", "https://untrusted.example".parse().unwrap());
        let denied_origin =
            handle_decision_import_pilot(State(state.clone()), wrong_origin, body.clone()).await;
        assert_eq!(denied_origin.status(), axum::http::StatusCode::FORBIDDEN);
        let malformed = Bytes::from_static(b"{\"raw-ticket-secret\":1}");
        let rejected =
            handle_decision_import_pilot(State(state.clone()), admin_headers.clone(), malformed)
                .await;
        assert_eq!(rejected.status(), axum::http::StatusCode::BAD_REQUEST);
        let rejected_body = axum::body::to_bytes(rejected.into_body(), 10_000)
            .await
            .unwrap();
        assert!(!String::from_utf8_lossy(&rejected_body).contains("raw-ticket-secret"));
        // Simulate a crash after all numerical inputs were written but before
        // the source-bound receipt. Both direct-ID APIs must fail closed.
        let store = DecisionStore::with_causal_store(
            state.home_dir.join("decisions.db"),
            CausalStore::new(state.home_dir.join("memory.db")),
        );
        let scope = DecisionScope {
            tenant_id: request.tenant_id.clone(),
            acl: request.acl.clone(),
        };
        let pilot = crate::decision_ingest::build_support_pilot(&request.export).unwrap();
        store.put_snapshot(&scope, &pilot.snapshot).unwrap();
        store.put_model(&scope, &request.model).unwrap();
        store.put_scenario(&scope, &pilot.baseline).unwrap();
        store
            .put_scenario(&scope, &request.alternative_scenario)
            .unwrap();
        let request_sha256 = format!(
            "{:x}",
            sha2::Sha256::digest(serde_json::to_vec(&request).unwrap())
        );
        store.open().unwrap().execute(
            "INSERT INTO decision_operator_pilot_imports
             (tenant_id,acl,snapshot_id,request_sha256,source_sha256,model_version,baseline_scenario_id,alternative_scenario_id)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            rusqlite::params![scope.tenant_id, scope.acl, pilot.snapshot.id, request_sha256,
                request.export.source_version_hashes[0], request.model.version,
                pilot.baseline.id, request.alternative_scenario.id],
        ).unwrap();
        let pending_replay = handle_decision_replay(
            State(state.clone()),
            admin_headers.clone(),
            decision_body(&DecisionReplayRequest {
                tenant_id: scope.tenant_id.clone(),
                acl: scope.acl.clone(),
                snapshot_id: pilot.snapshot.id.clone(),
                model_version: request.model.version.clone(),
                scenario_id: pilot.baseline.id.clone(),
                expected_hash: "0".repeat(64),
            }),
        )
        .await;
        assert_eq!(pending_replay.status(), axum::http::StatusCode::NOT_FOUND);
        let pending_compare = handle_decision_compare(
            State(state.clone()),
            admin_headers.clone(),
            decision_body(&DecisionCompareRequest {
                tenant_id: scope.tenant_id.clone(),
                acl: scope.acl.clone(),
                snapshot_id: pilot.snapshot.id.clone(),
                model_version: request.model.version.clone(),
                baseline_scenario_id: pilot.baseline.id.clone(),
                alternative_scenario_id: request.alternative_scenario.id.clone(),
                empirical_run_id: None,
                policy_screen_hash: None,
                baseline_event_replay_hash: None,
                alternative_event_replay_hash: None,
                sla_holdout_id: None,
                forecast_validation_id: None,
                effect_ids: Vec::new(),
            }),
        )
        .await;
        assert_eq!(pending_compare.status(), axum::http::StatusCode::NOT_FOUND);
        let pending_sla_compare = handle_decision_compare(
            State(state.clone()),
            admin_headers.clone(),
            decision_body(&DecisionCompareRequest {
                tenant_id: scope.tenant_id.clone(),
                acl: scope.acl.clone(),
                snapshot_id: pilot.snapshot.id.clone(),
                model_version: request.model.version.clone(),
                baseline_scenario_id: pilot.baseline.id.clone(),
                alternative_scenario_id: request.alternative_scenario.id.clone(),
                empirical_run_id: None,
                policy_screen_hash: None,
                baseline_event_replay_hash: None,
                alternative_event_replay_hash: None,
                sla_holdout_id: Some("pending-holdout".into()),
                forecast_validation_id: None,
                effect_ids: Vec::new(),
            }),
        )
        .await;
        assert_eq!(
            pending_sla_compare.status(),
            axum::http::StatusCode::NOT_FOUND
        );
        let pending_forecast_compare = handle_decision_compare(
            State(state.clone()),
            admin_headers.clone(),
            decision_body(&DecisionCompareRequest {
                tenant_id: scope.tenant_id.clone(),
                acl: scope.acl.clone(),
                snapshot_id: pilot.snapshot.id.clone(),
                model_version: request.model.version.clone(),
                baseline_scenario_id: pilot.baseline.id.clone(),
                alternative_scenario_id: request.alternative_scenario.id.clone(),
                empirical_run_id: None,
                policy_screen_hash: None,
                baseline_event_replay_hash: None,
                alternative_event_replay_hash: None,
                sla_holdout_id: None,
                forecast_validation_id: Some("pending-forecast".into()),
                effect_ids: Vec::new(),
            }),
        )
        .await;
        assert_eq!(
            pending_forecast_compare.status(),
            axum::http::StatusCode::NOT_FOUND
        );
        let pending_event_compare = handle_decision_compare(
            State(state.clone()),
            admin_headers.clone(),
            decision_body(&DecisionCompareRequest {
                tenant_id: scope.tenant_id.clone(),
                acl: scope.acl.clone(),
                snapshot_id: pilot.snapshot.id.clone(),
                model_version: request.model.version.clone(),
                baseline_scenario_id: pilot.baseline.id.clone(),
                alternative_scenario_id: request.alternative_scenario.id.clone(),
                empirical_run_id: Some("pending-empirical".into()),
                policy_screen_hash: None,
                baseline_event_replay_hash: Some("0".repeat(64)),
                alternative_event_replay_hash: Some("1".repeat(64)),
                sla_holdout_id: None,
                forecast_validation_id: None,
                effect_ids: Vec::new(),
            }),
        )
        .await;
        assert_eq!(
            pending_event_compare.status(),
            axum::http::StatusCode::NOT_FOUND
        );
        let accepted =
            handle_decision_import_pilot(State(state.clone()), admin_headers.clone(), body.clone())
                .await;
        assert_eq!(accepted.status(), axum::http::StatusCode::OK);
        let accepted_body = axum::body::to_bytes(accepted.into_body(), 100_000)
            .await
            .unwrap();
        let receipt: serde_json::Value = serde_json::from_slice(&accepted_body).unwrap();
        assert_eq!(receipt["status"], "exploratory_operator_upload");
        assert!(receipt["sla_holdout_id"].as_str().is_some());
        assert!(receipt["sla_holdout_sha256"].as_str().is_some());
        assert!(!String::from_utf8_lossy(&accepted_body).contains("day-0-ticket-0"));
        let retried =
            handle_decision_import_pilot(State(state.clone()), admin_headers.clone(), body).await;
        assert_eq!(retried.status(), axum::http::StatusCode::OK);
        let catalog = handle_decision_catalog(
            State(state.clone()),
            admin_headers.clone(),
            decision_raw_query(&DecisionCatalogQuery {
                tenant_id: "tenant-a".into(),
                acl: "private".into(),
            }),
        )
        .await;
        assert_eq!(catalog.status(), axum::http::StatusCode::OK);
        let catalog_body = axum::body::to_bytes(catalog.into_body(), 100_000)
            .await
            .unwrap();
        let catalog_value: serde_json::Value = serde_json::from_slice(&catalog_body).unwrap();
        assert_eq!(
            catalog_value["uploaded_pilots"][0]["snapshot_id"],
            receipt["snapshot_id"]
        );
        let diagnostics = handle_decision_engineering_validation(
            State(state.clone()),
            admin_headers.clone(),
            decision_body(&DecisionEngineeringValidationRequest {
                tenant_id: scope.tenant_id.clone(),
                acl: scope.acl.clone(),
                snapshot_id: pilot.snapshot.id.clone(),
                model_version: request.model.version.clone(),
            }),
        )
        .await;
        assert_eq!(diagnostics.status(), axum::http::StatusCode::OK);
        let diagnostics_body = axum::body::to_bytes(diagnostics.into_body(), 100_000)
            .await
            .unwrap();
        let diagnostics_value: serde_json::Value =
            serde_json::from_slice(&diagnostics_body).unwrap();
        assert_eq!(
            diagnostics_value["report"]["status"],
            "exploratory_operator_upload"
        );
        assert_eq!(
            diagnostics_value["report"]["source_version_sha256"],
            receipt["source_sha256"]
        );
        assert!(!String::from_utf8_lossy(&diagnostics_body).contains("day-0-ticket-0"));
        let forecast_request = |tenant_id: &str, acl: &str| DecisionForecastValidationRequest {
            tenant_id: tenant_id.into(),
            acl: acl.into(),
            snapshot_id: pilot.snapshot.id.clone(),
            model_version: request.model.version.clone(),
            baseline_scenario_id: pilot.baseline.id.clone(),
            alternative_scenario_id: request.alternative_scenario.id.clone(),
        };
        assert_eq!(
            handle_decision_forecast_validation(
                State(state.clone()),
                employee_headers.clone(),
                decision_body(&forecast_request("tenant-a", "private")),
            )
            .await
            .status(),
            axum::http::StatusCode::FORBIDDEN
        );
        assert_ne!(
            handle_decision_forecast_validation(
                State(state.clone()),
                admin_headers.clone(),
                decision_body(&forecast_request("other-tenant", "private")),
            )
            .await
            .status(),
            axum::http::StatusCode::OK
        );
        let forecast_response = handle_decision_forecast_validation(
            State(state.clone()),
            admin_headers.clone(),
            decision_body(&forecast_request("tenant-a", "private")),
        )
        .await;
        assert_eq!(forecast_response.status(), axum::http::StatusCode::OK);
        let forecast_body = axum::body::to_bytes(forecast_response.into_body(), 100_000)
            .await
            .unwrap();
        let forecast_value: serde_json::Value = serde_json::from_slice(&forecast_body).unwrap();
        assert_eq!(
            forecast_value["report"]["status"],
            "operator_supplied_exploratory"
        );
        assert_eq!(forecast_value["report"]["source_origin"], "uploaded");
        assert!(forecast_value["report"]["record_id"].as_str().is_some());
        assert!(forecast_value["report"]["record_sha256"].as_str().is_some());
        // Error sums are u128 on the store side and travel as decimal strings
        // so the browser cannot lose precision past 2^53.
        assert!(
            forecast_value["report"]["model_abs_error_sum"]
                .as_str()
                .is_some_and(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        );
        assert!(!String::from_utf8_lossy(&forecast_body).contains("day-0-ticket-0"));
        let fit = crate::decision_empirical::fit_empirical_parameters(&pilot.observed_days, 14, 7)
            .unwrap();
        let fallback = (fit.capacity_identification
            == crate::decision_empirical::CapacityIdentification::Unidentified)
            .then_some(BoundedCount { min: 7, max: 9 });
        let empirical = handle_decision_empirical_resampling(
            State(state.clone()),
            admin_headers.clone(),
            decision_body(&DecisionEmpiricalResamplingRequest {
                tenant_id: scope.tenant_id.clone(),
                acl: scope.acl.clone(),
                snapshot_id: pilot.snapshot.id.clone(),
                model_version: request.model.version.clone(),
                baseline_scenario_id: pilot.baseline.id.clone(),
                alternative_scenario_id: request.alternative_scenario.id.clone(),
                training_days: 14,
                min_saturated_days: 7,
                runs: 8,
                arrival_block_days: 1,
                sampling_mode: EmpiricalSamplingMode::Independent,
                capacity_fallback_range: fallback,
                max_final_backlog: 500,
                max_staff_cost_cents: 1_000_000,
                min_sla_resolved: Some(0),
                screen: None,
            }),
        )
        .await;
        assert_eq!(empirical.status(), axum::http::StatusCode::OK);
        let empirical_body = axum::body::to_bytes(empirical.into_body(), 100_000)
            .await
            .unwrap();
        let empirical_value: serde_json::Value = serde_json::from_slice(&empirical_body).unwrap();
        assert_eq!(empirical_value["status"], "exploratory_operator_upload");
        let event = handle_decision_event_compare(
            State(state.clone()),
            admin_headers.clone(),
            decision_body(&DecisionEventCompareRequest {
                tenant_id: scope.tenant_id.clone(),
                acl: scope.acl.clone(),
                snapshot_id: pilot.snapshot.id.clone(),
                model_version: request.model.version.clone(),
                baseline_scenario_id: pilot.baseline.id.clone(),
                alternative_scenario_id: request.alternative_scenario.id.clone(),
            }),
        )
        .await;
        assert_eq!(event.status(), axum::http::StatusCode::OK);
        let event_body = axum::body::to_bytes(event.into_body(), 100_000)
            .await
            .unwrap();
        let event_value: serde_json::Value = serde_json::from_slice(&event_body).unwrap();
        assert_eq!(event_value["report"]["source_origin"], "uploaded");
        let uploaded_compare_request = || DecisionCompareRequest {
            tenant_id: scope.tenant_id.clone(),
            acl: scope.acl.clone(),
            snapshot_id: pilot.snapshot.id.clone(),
            model_version: request.model.version.clone(),
            baseline_scenario_id: pilot.baseline.id.clone(),
            alternative_scenario_id: request.alternative_scenario.id.clone(),
            empirical_run_id: Some(empirical_value["run"]["id"].as_str().unwrap().into()),
            policy_screen_hash: None,
            baseline_event_replay_hash: Some(
                event_value["report"]["baseline"]["replay_hash"]
                    .as_str()
                    .unwrap()
                    .into(),
            ),
            alternative_event_replay_hash: Some(
                event_value["report"]["alternative"]["replay_hash"]
                    .as_str()
                    .unwrap()
                    .into(),
            ),
            sla_holdout_id: Some(receipt["sla_holdout_id"].as_str().unwrap().into()),
            forecast_validation_id: Some(
                forecast_value["report"]["record_id"]
                    .as_str()
                    .unwrap()
                    .into(),
            ),
            effect_ids: Vec::new(),
        };
        let uploaded_brief = handle_decision_compare(
            State(state.clone()),
            admin_headers.clone(),
            decision_body(&uploaded_compare_request()),
        )
        .await;
        assert_eq!(uploaded_brief.status(), axum::http::StatusCode::OK);
        let uploaded_brief_body = axum::body::to_bytes(uploaded_brief.into_body(), 100_000)
            .await
            .unwrap();
        let uploaded_brief_value: serde_json::Value =
            serde_json::from_slice(&uploaded_brief_body).unwrap();
        assert_eq!(
            uploaded_brief_value["brief"]["exploratory_event"]["source_sha256"],
            receipt["source_sha256"]
        );
        assert_eq!(
            uploaded_brief_value["brief"]["exploratory_event"]["baseline_run_hash"],
            event_value["report"]["baseline"]["replay_hash"]
        );
        assert_eq!(
            uploaded_brief_value["brief"]["exploratory_sla_holdout"]["record_id"],
            receipt["sla_holdout_id"]
        );
        assert_eq!(
            uploaded_brief_value["brief"]["exploratory_sla_holdout"]["record_sha256"],
            receipt["sla_holdout_sha256"]
        );
        assert_eq!(
            uploaded_brief_value["brief"]["exploratory_forecast"]["record_id"],
            forecast_value["report"]["record_id"]
        );
        // Both views of the same stored record ship the sum as one decimal
        // string, so they compare directly.
        assert_eq!(
            uploaded_brief_value["brief"]["exploratory_forecast"]["model_abs_error_sum"],
            forecast_value["report"]["model_abs_error_sum"]
        );
        assert!(!String::from_utf8_lossy(&uploaded_brief_body).contains("day-0-ticket-0"));
        let mut sla_only = uploaded_compare_request();
        sla_only.empirical_run_id = None;
        sla_only.baseline_event_replay_hash = None;
        sla_only.alternative_event_replay_hash = None;
        sla_only.forecast_validation_id = None;
        let sla_only_response =
            handle_decision_compare(State(state.clone()), admin_headers.clone(), decision_body(&sla_only))
                .await;
        assert_eq!(sla_only_response.status(), axum::http::StatusCode::OK);
        let sla_only_body = axum::body::to_bytes(sla_only_response.into_body(), 100_000)
            .await
            .unwrap();
        let sla_only_value: serde_json::Value = serde_json::from_slice(&sla_only_body).unwrap();
        assert_eq!(
            sla_only_value["brief"]["exploratory_sla_holdout"]["record_id"],
            receipt["sla_holdout_id"]
        );
        assert!(sla_only_value["brief"]["exploratory_empirical"].is_null());
        assert!(sla_only_value["brief"]["exploratory_event"].is_null());
        let mut forecast_only = uploaded_compare_request();
        forecast_only.empirical_run_id = None;
        forecast_only.baseline_event_replay_hash = None;
        forecast_only.alternative_event_replay_hash = None;
        forecast_only.sla_holdout_id = None;
        let forecast_only_response = handle_decision_compare(
            State(state.clone()),
            admin_headers.clone(),
            decision_body(&forecast_only),
        )
        .await;
        assert_eq!(forecast_only_response.status(), axum::http::StatusCode::OK);
        let forecast_only_body = axum::body::to_bytes(forecast_only_response.into_body(), 100_000)
            .await
            .unwrap();
        let forecast_only_value: serde_json::Value =
            serde_json::from_slice(&forecast_only_body).unwrap();
        assert_eq!(
            forecast_only_value["brief"]["exploratory_forecast"]["record_id"],
            forecast_value["report"]["record_id"]
        );
        assert!(forecast_only_value["brief"]["exploratory_empirical"].is_null());
        assert!(forecast_only_value["brief"]["exploratory_sla_holdout"].is_null());
        let mut wrong_forecast = uploaded_compare_request();
        wrong_forecast.forecast_validation_id = Some("wrong-forecast".into());
        assert_eq!(
            handle_decision_compare(
                State(state.clone()),
                admin_headers.clone(),
                decision_body(&wrong_forecast),
            )
            .await
            .status(),
            axum::http::StatusCode::BAD_REQUEST
        );
        let mut wrong_holdout = uploaded_compare_request();
        wrong_holdout.sla_holdout_id = Some("wrong-holdout".into());
        assert_eq!(
            handle_decision_compare(
                State(state.clone()),
                admin_headers.clone(),
                decision_body(&wrong_holdout),
            )
            .await
            .status(),
            axum::http::StatusCode::BAD_REQUEST
        );
        let mut wrong_pair = uploaded_compare_request();
        wrong_pair.alternative_scenario_id = wrong_pair.baseline_scenario_id.clone();
        assert_ne!(
            handle_decision_compare(
                State(state.clone()),
                admin_headers.clone(),
                decision_body(&wrong_pair),
            )
            .await
            .status(),
            axum::http::StatusCode::OK
        );
        store
            .open()
            .unwrap()
            .execute(
                "UPDATE decision_inputs SET payload_sha256='0'
             WHERE tenant_id=?1 AND acl=?2 AND kind='sla_holdout' AND input_id=?3",
                rusqlite::params![
                    scope.tenant_id,
                    scope.acl,
                    receipt["sla_holdout_id"].as_str().unwrap()
                ],
            )
            .unwrap();
        let tampered = handle_decision_compare(
            State(state.clone()),
            admin_headers.clone(),
            decision_body(&uploaded_compare_request()),
        )
        .await;
        assert_ne!(tampered.status(), axum::http::StatusCode::OK);
        store
            .open()
            .unwrap()
            .execute(
                "UPDATE decision_inputs SET payload_sha256=?1
             WHERE tenant_id=?2 AND acl=?3 AND kind='sla_holdout' AND input_id=?4",
                rusqlite::params![
                    receipt["sla_holdout_sha256"].as_str().unwrap(),
                    scope.tenant_id,
                    scope.acl,
                    receipt["sla_holdout_id"].as_str().unwrap()
                ],
            )
            .unwrap();
        let mut wrong_scope = uploaded_compare_request();
        wrong_scope.acl = "staff".into();
        assert_ne!(
            handle_decision_compare(
                State(state.clone()),
                admin_headers.clone(),
                decision_body(&wrong_scope)
            )
            .await
            .status(),
            axum::http::StatusCode::OK
        );
        assert_eq!(
            handle_decision_compare(
                State(state.clone()),
                employee_headers.clone(),
                decision_body(&uploaded_compare_request()),
            )
            .await
            .status(),
            axum::http::StatusCode::FORBIDDEN
        );
        let inspection = handle_decision_pilot_review_request(
            State(state.clone()),
            admin_headers.clone(),
            decision_body(&DecisionPilotReviewRequest {
                tenant_id: scope.tenant_id.clone(),
                acl: scope.acl.clone(),
                snapshot_id: pilot.snapshot.id.clone(),
                model_version: request.model.version.clone(),
                scenario_id: pilot.baseline.id.clone(),
                expected_hash: receipt["baseline_replay_hash"].as_str().unwrap().into(),
                summary: "Inspect imported source assumptions".into(),
                ttl_seconds: 3600,
            }),
        )
        .await;
        assert_eq!(inspection.status(), axum::http::StatusCode::OK);
        let inspection_body = axum::body::to_bytes(inspection.into_body(), 100_000)
            .await
            .unwrap();
        let inspection_value: serde_json::Value = serde_json::from_slice(&inspection_body).unwrap();
        assert_eq!(inspection_value["review"]["status"], "pending");
        let status = handle_decision_pilot_review_status(
            State(state.clone()),
            admin_headers.clone(),
            decision_body(&DecisionPilotReviewStatusRequest {
                tenant_id: scope.tenant_id.clone(),
                acl: scope.acl.clone(),
                snapshot_id: pilot.snapshot.id.clone(),
                model_version: request.model.version.clone(),
                scenario_id: pilot.baseline.id.clone(),
                expected_hash: receipt["baseline_replay_hash"].as_str().unwrap().into(),
                approval_id: inspection_value["review"]["link"]["approval_id"]
                    .as_str()
                    .unwrap()
                    .into(),
            }),
        )
        .await;
        assert_eq!(status.status(), axum::http::StatusCode::OK);
        CausalStore::new(state.home_dir.join("memory.db"))
            .invalidate_artifact(
                &EvidenceScope {
                    tenant_id: scope.tenant_id.clone(),
                    acl: scope.acl.clone(),
                },
                receipt["source_artifact_id"].as_str().unwrap(),
            )
            .unwrap();
        assert_ne!(
            handle_decision_compare(
                State(state),
                admin_headers,
                decision_body(&uploaded_compare_request()),
            )
            .await
            .status(),
            axum::http::StatusCode::OK
        );
    }

    #[tokio::test]
    async fn ccr_replay_requires_admin_and_exact_tenant_without_echoing_material() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();
        let user_db = Arc::new(UserDb::new(&home.join("users.db")).unwrap());
        let admin = user_db
            .create_user(
                "ccr-admin@example.test",
                "Admin",
                "test-password",
                UserRole::Admin,
            )
            .unwrap();
        let employee = user_db
            .create_user(
                "ccr-employee@example.test",
                "Employee",
                "test-password",
                UserRole::Employee,
            )
            .unwrap();
        let jwt_config = Arc::new(JwtConfig::new(b"ccr-replay-test-secret-32-bytes!!!"));
        let admin_token = jwt_config.issue_access_token(&admin, &[]).unwrap();
        let employee_token = jwt_config.issue_access_token(&employee, &[]).unwrap();
        let (tx, _) = broadcast::channel(4);
        let (event_tx, _) = broadcast::channel(4);
        let state = Arc::new(AppState {
            auth: AuthManager::new(None),
            handler: MethodHandler::new(home.clone()).await,
            tx,
            event_tx,
            user_db,
            jwt_config,
            otp_delivery: Arc::new(crate::otp_delivery::ConfigOtpDeliverer::new(
                home.clone(),
                reqwest::Client::new(),
            )),
            home_dir: home,
        });
        let mut admin_headers = axum::http::HeaderMap::new();
        admin_headers.insert(
            "authorization",
            format!("Bearer {admin_token}").parse().unwrap(),
        );
        admin_headers.insert("origin", "http://localhost:18789".parse().unwrap());
        let mut employee_headers = admin_headers.clone();
        employee_headers.insert(
            "authorization",
            format!("Bearer {employee_token}").parse().unwrap(),
        );
        let fixture = include_str!("../../../fixtures/ccr/native-replay-synthetic.json");
        let query = || {
            Query(CcrDashboardQuery {
                tenant_id: "local".into(),
            })
        };

        let denied = handle_ccr_replay(
            State(state.clone()),
            employee_headers,
            query(),
            axum::body::Bytes::from(fixture),
        )
        .await;
        assert_eq!(denied.status(), axum::http::StatusCode::FORBIDDEN);

        let expected =
            crate::ccr_replay::evaluate_replay_bytes(fixture.as_bytes(), Some("local")).unwrap();
        let success = handle_ccr_replay(
            State(state.clone()),
            admin_headers.clone(),
            query(),
            axum::body::Bytes::from(fixture),
        )
        .await;
        assert_eq!(success.status(), axum::http::StatusCode::OK);
        let success_body = axum::body::to_bytes(success.into_body(), usize::MAX)
            .await
            .unwrap();
        let actual: serde_json::Value = serde_json::from_slice(&success_body).unwrap();
        assert_eq!(actual, expected);
        assert!(!String::from_utf8_lossy(&success_body).contains("Find the exact ticket value"));

        let mut mismatched: serde_json::Value = serde_json::from_str(fixture).unwrap();
        mismatched["tasks"][0]["arms"][1]["authorization"]["tenant_id"] =
            "secret-customer-tenant".into();
        let mismatch = handle_ccr_replay(
            State(state.clone()),
            admin_headers.clone(),
            query(),
            axum::body::Bytes::from(mismatched.to_string()),
        )
        .await;
        assert_eq!(mismatch.status(), axum::http::StatusCode::BAD_REQUEST);
        let mismatch_body = axum::body::to_bytes(mismatch.into_body(), usize::MAX)
            .await
            .unwrap();
        assert!(!String::from_utf8_lossy(&mismatch_body).contains("secret-customer-tenant"));

        let mut changed_source: serde_json::Value = serde_json::from_str(fixture).unwrap();
        changed_source["tasks"][0]["arms"][1]["source_result"] =
            "Private source EXACT-001 changed".into();
        let invalid = handle_ccr_replay(
            State(state.clone()),
            admin_headers.clone(),
            query(),
            axum::body::Bytes::from(changed_source.to_string()),
        )
        .await;
        assert_eq!(invalid.status(), axum::http::StatusCode::BAD_REQUEST);
        let invalid_body = axum::body::to_bytes(invalid.into_body(), usize::MAX)
            .await
            .unwrap();
        assert!(!String::from_utf8_lossy(&invalid_body).contains("Private source"));

        let mut malformed: serde_json::Value = serde_json::from_str(fixture).unwrap();
        malformed["Private customer ticket label"] = serde_json::json!("secret");
        let malformed_response = handle_ccr_replay(
            State(state),
            admin_headers,
            query(),
            axum::body::Bytes::from(malformed.to_string()),
        )
        .await;
        assert_eq!(
            malformed_response.status(),
            axum::http::StatusCode::BAD_REQUEST
        );
        let malformed_body = axum::body::to_bytes(malformed_response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert!(
            !String::from_utf8_lossy(&malformed_body).contains("Private customer ticket label")
        );
    }

    #[test]
    fn claim_revision_json_deserializes_with_flattened_fields() {
        let parsed: CausalClaimReviseRequest = serde_json::from_value(serde_json::json!({
            "tenant_id": "t", "acl": "private", "id": "claim",
            "expected_state": "accepted", "cause_variable": "A",
            "effect_variable": "B", "lag_min_seconds": 0,
            "lag_max_seconds": 1, "modality": "asserted",
            "context": {}, "evidence_ids": ["span"], "note": "correction"
        }))
        .unwrap();
        assert_eq!(parsed.revision.evidence_ids, ["span"]);
    }

    #[test]
    fn negative_control_review_requires_a_declared_verdict_and_no_reviewer_override() {
        let valid = serde_json::json!({
            "tenant_id": "tenant-a", "acl": "private", "model_id": "model",
            "variable_id": "control", "protocol_artifact_id": "protocol",
            "verdict": "pass", "rationale": "Independent exclusion review completed"
        });
        let request: CausalNegativeControlReviewRequest =
            serde_json::from_value(valid.clone()).unwrap();
        assert_eq!(
            request.verdict,
            duduclaw_memory::causal_model::AssumptionVerdict::Pass
        );
        let mut forged = valid;
        forged["reviewer"] = serde_json::json!("someone-else");
        assert!(serde_json::from_value::<CausalNegativeControlReviewRequest>(forged).is_err());
    }

    #[test]
    fn source_chunks_are_bounded_and_advance_on_utf8_boundaries() {
        let source = "甲乙丙";
        let first = bounded_causal_source(source, 1, 4);
        assert_eq!(first["text"], "乙");
        assert_eq!(first["byte_offset"], 3);
        assert_eq!(first["next_offset"], 6);
        assert_eq!(first["truncated"], true);
        let second = bounded_causal_source(source, 6, 4);
        assert_eq!(second["text"], "丙");
        assert_eq!(second["truncated"], false);

        let large = "x".repeat(100_000);
        let chunk = bounded_causal_source(&large, 0, usize::MAX);
        assert_eq!(chunk["text"].as_str().unwrap().len(), 64 * 1024);
        assert_eq!(chunk["next_offset"], 64 * 1024);
    }

    #[tokio::test]
    async fn extraction_eval_requires_admin_and_exact_active_case_sources() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();
        let user_db = Arc::new(UserDb::new(&home.join("users.db")).unwrap());
        let admin = user_db
            .create_user(
                "eval-admin@example.test",
                "Admin",
                "test-password",
                UserRole::Admin,
            )
            .unwrap();
        let employee = user_db
            .create_user(
                "eval-employee@example.test",
                "Employee",
                "test-password",
                UserRole::Employee,
            )
            .unwrap();
        let jwt_config = Arc::new(JwtConfig::new(b"causal-evaluation-test-secret-32-bytes"));
        let admin_token = jwt_config.issue_access_token(&admin, &[]).unwrap();
        let employee_token = jwt_config.issue_access_token(&employee, &[]).unwrap();
        let (tx, _) = broadcast::channel(4);
        let (event_tx, _) = broadcast::channel(4);
        let state = Arc::new(AppState {
            auth: AuthManager::new(None),
            handler: MethodHandler::new(home.clone()).await,
            tx,
            event_tx,
            user_db,
            jwt_config,
            otp_delivery: Arc::new(crate::otp_delivery::ConfigOtpDeliverer::new(
                home.clone(),
                reqwest::Client::new(),
            )),
            home_dir: home.clone(),
        });
        let mut admin_headers = axum::http::HeaderMap::new();
        admin_headers.insert(
            "authorization",
            format!("Bearer {admin_token}").parse().unwrap(),
        );
        admin_headers.insert("origin", "http://localhost:18789".parse().unwrap());
        let mut employee_headers = admin_headers.clone();
        employee_headers.insert(
            "authorization",
            format!("Bearer {employee_token}").parse().unwrap(),
        );
        let synthetic = || CausalExtractionEvalRequest::BundledSynthetic {
            tenant_id: "tenant-a".into(),
            acl: "private".into(),
        };
        let denied = handle_causal_extraction_eval(
            State(state.clone()),
            employee_headers,
            Json(synthetic()),
        )
        .await;
        assert_eq!(denied.status(), axum::http::StatusCode::FORBIDDEN);
        let bundled = handle_causal_extraction_eval(
            State(state.clone()),
            admin_headers.clone(),
            Json(synthetic()),
        )
        .await;
        assert_eq!(bundled.status(), axum::http::StatusCode::OK);
        let body = axum::body::to_bytes(bundled.into_body(), 32_000)
            .await
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["source_provenance"], "bundled_synthetic_unbound");
        assert_eq!(value["report"]["held_out_families"], 3);

        let dataset = || {
            serde_json::from_str::<duduclaw_memory::causal_eval::ExtractionEvalDataset>(
                include_str!("../../../fixtures/causal-extraction-synthetic.json"),
            )
            .unwrap()
        };
        let scope = EvidenceScope {
            tenant_id: "tenant-a".into(),
            acl: "private".into(),
        };
        let store = CausalStore::new(home.join("memory.db"));
        let mut bindings = std::collections::BTreeMap::new();
        for case in &dataset().cases {
            let artifact = store
                .add_artifact(
                    &scope,
                    "ticket",
                    &case.id,
                    "v1",
                    &case.source_lineage_id,
                    &case.source_text,
                    case.occurred_at,
                    i64::MAX,
                )
                .unwrap();
            bindings.insert(case.id.clone(), artifact.id);
        }
        let scoped = |acl: &str, case_artifacts| CausalExtractionEvalRequest::ScopedDataset {
            tenant_id: "tenant-a".into(),
            acl: acl.into(),
            dataset: dataset(),
            case_artifacts,
        };
        let evaluated = handle_causal_extraction_eval(
            State(state.clone()),
            admin_headers.clone(),
            Json(scoped("private", bindings.clone())),
        )
        .await;
        assert_eq!(evaluated.status(), axum::http::StatusCode::OK);
        let body = axum::body::to_bytes(evaluated.into_body(), 32_000)
            .await
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["scope"]["acl"], "private");
        assert_eq!(
            value["source_provenance"],
            "active_scoped_artifacts_at_evaluation"
        );
        assert_eq!(value["family_provenance"], "caller_supplied_unverified");
        assert_eq!(value["report"]["held_out_families"], 3);
        let wrong_acl = handle_causal_extraction_eval(
            State(state.clone()),
            admin_headers.clone(),
            Json(scoped("other", bindings.clone())),
        )
        .await;
        assert_eq!(wrong_acl.status(), axum::http::StatusCode::CONFLICT);
        let mut missing = bindings.clone();
        missing.pop_last();
        let incomplete = handle_causal_extraction_eval(
            State(state.clone()),
            admin_headers.clone(),
            Json(scoped("private", missing)),
        )
        .await;
        assert_eq!(incomplete.status(), axum::http::StatusCode::BAD_REQUEST);
        let mut extra = bindings.clone();
        extra.insert("extra".into(), "nonexistent".into());
        let overspecified = handle_causal_extraction_eval(
            State(state.clone()),
            admin_headers.clone(),
            Json(scoped("private", extra)),
        )
        .await;
        assert_eq!(overspecified.status(), axum::http::StatusCode::BAD_REQUEST);
        let first_artifact = bindings.values().next().unwrap();
        store.invalidate_artifact(&scope, first_artifact).unwrap();
        let revoked = handle_causal_extraction_eval(
            State(state),
            admin_headers,
            Json(scoped("private", bindings)),
        )
        .await;
        assert_eq!(revoked.status(), axum::http::StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn curation_requires_admin_and_rejects_stale_review() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();
        let user_db = Arc::new(UserDb::new(&home.join("users.db")).unwrap());
        let admin = user_db
            .create_user(
                "admin@example.test",
                "Admin",
                "test-password",
                UserRole::Admin,
            )
            .unwrap();
        let employee = user_db
            .create_user(
                "employee@example.test",
                "Employee",
                "test-password",
                UserRole::Employee,
            )
            .unwrap();
        let jwt_config = Arc::new(JwtConfig::new(b"causal-curation-test-secret-32-bytes"));
        let admin_token = jwt_config.issue_access_token(&admin, &[]).unwrap();
        let employee_token = jwt_config.issue_access_token(&employee, &[]).unwrap();
        let (tx, _) = broadcast::channel(4);
        let (event_tx, _) = broadcast::channel(4);
        let state = Arc::new(AppState {
            auth: AuthManager::new(None),
            handler: MethodHandler::new(home.clone()).await,
            tx,
            event_tx,
            user_db,
            jwt_config,
            otp_delivery: Arc::new(crate::otp_delivery::ConfigOtpDeliverer::new(
                home.clone(),
                reqwest::Client::new(),
            )),
            home_dir: home.clone(),
        });
        let mut admin_headers = axum::http::HeaderMap::new();
        admin_headers.insert(
            "authorization",
            format!("Bearer {admin_token}").parse().unwrap(),
        );
        admin_headers.insert("origin", "http://localhost:18789".parse().unwrap());
        let mut employee_headers = admin_headers.clone();
        employee_headers.insert(
            "authorization",
            format!("Bearer {employee_token}").parse().unwrap(),
        );
        let query = CausalClaimsQuery {
            tenant_id: "tenant-a".into(),
            acl: "private".into(),
            review_state: None,
            limit: None,
        };
        let denied = handle_causal_claims(
            State(state.clone()),
            employee_headers.clone(),
            Query(CausalClaimsQuery {
                tenant_id: query.tenant_id.clone(),
                acl: query.acl.clone(),
                review_state: None,
                limit: None,
            }),
        )
        .await;
        assert_eq!(denied.status(), axum::http::StatusCode::FORBIDDEN);
        let missing = handle_causal_claims(
            State(state.clone()),
            axum::http::HeaderMap::new(),
            Query(CausalClaimsQuery {
                tenant_id: query.tenant_id.clone(),
                acl: query.acl.clone(),
                review_state: None,
                limit: None,
            }),
        )
        .await;
        assert_eq!(missing.status(), axum::http::StatusCode::UNAUTHORIZED);
        let mut wrong_origin = admin_headers.clone();
        wrong_origin.insert("origin", "https://localhost.evil.test".parse().unwrap());
        let origin_denied =
            handle_causal_claims(State(state.clone()), wrong_origin, Query(query)).await;
        assert_eq!(origin_denied.status(), axum::http::StatusCode::FORBIDDEN);

        let decision_denied = handle_decision_overview(
            State(state.clone()),
            employee_headers.clone(),
            decision_raw_query(&DecisionOverviewQuery {
                tenant_id: "tenant-a".into(),
                acl: "private".into(),
                limit: Some(10),
            }),
        )
        .await;
        assert_eq!(decision_denied.status(), axum::http::StatusCode::FORBIDDEN);
        let decision_overview = handle_decision_overview(
            State(state.clone()),
            admin_headers.clone(),
            decision_raw_query(&DecisionOverviewQuery {
                tenant_id: "tenant-a".into(),
                acl: "private".into(),
                limit: Some(10),
            }),
        )
        .await;
        assert_eq!(decision_overview.status(), axum::http::StatusCode::OK);
        let ccr_denied = handle_ccr_dashboard(
            State(state.clone()),
            employee_headers.clone(),
            Query(CcrDashboardQuery {
                tenant_id: "local".into(),
            }),
        )
        .await;
        assert_eq!(ccr_denied.status(), axum::http::StatusCode::FORBIDDEN);
        let ccr_ready_or_absent = handle_ccr_dashboard(
            State(state.clone()),
            admin_headers.clone(),
            Query(CcrDashboardQuery {
                tenant_id: "local".into(),
            }),
        )
        .await;
        assert_eq!(ccr_ready_or_absent.status(), axum::http::StatusCode::OK);
        let ccr_invalid_scope = handle_ccr_dashboard(
            State(state.clone()),
            admin_headers.clone(),
            Query(CcrDashboardQuery {
                tenant_id: " ".into(),
            }),
        )
        .await;
        assert_eq!(
            ccr_invalid_scope.status(),
            axum::http::StatusCode::BAD_REQUEST
        );
        let scrubbed = handle_decision_ticket_sources_scrub(
            State(state.clone()),
            admin_headers.clone(),
            decision_body(&DecisionTicketSourcesScrubRequest {
                tenant_id: "tenant-a".into(),
                acl: "private".into(),
            }),
        )
        .await;
        assert_eq!(scrubbed.status(), axum::http::StatusCode::OK);

        let denied_demo = handle_decision_synthetic_pilot(
            State(state.clone()),
            employee_headers.clone(),
            decision_body(&DecisionSyntheticPilotRequest {
                tenant_id: "tenant-a".into(),
                acl: "private".into(),
                seed: 47,
                days: 35,
            }),
        )
        .await;
        assert_eq!(denied_demo.status(), axum::http::StatusCode::FORBIDDEN);
        let created_demo = handle_decision_synthetic_pilot(
            State(state.clone()),
            admin_headers.clone(),
            decision_body(&DecisionSyntheticPilotRequest {
                tenant_id: "tenant-a".into(),
                acl: "private".into(),
                seed: 47,
                days: 35,
            }),
        )
        .await;
        assert_eq!(created_demo.status(), axum::http::StatusCode::OK);
        let empirical_request = |tenant_id: &str, acl: &str| DecisionEmpiricalResamplingRequest {
            tenant_id: tenant_id.into(),
            acl: acl.into(),
            snapshot_id: "synthetic-support-47-35".into(),
            model_version: "dashboard-synthetic-capacity-v1-47-35".into(),
            baseline_scenario_id: "dashboard-synthetic-baseline-two-agents-47-35".into(),
            alternative_scenario_id: "dashboard-synthetic-three-agents-47-35".into(),
            training_days: 21,
            min_saturated_days: 7,
            runs: 8,
            arrival_block_days: 1,
            sampling_mode: EmpiricalSamplingMode::Independent,
            capacity_fallback_range: None,
            max_final_backlog: 300,
            max_staff_cost_cents: 1_100_000,
            min_sla_resolved: Some(0),
            screen: Some(DecisionEmpiricalScreenRequest {
                resource_plan: StaffingResourcePlan {
                    available_agents_by_day: vec![3; 35],
                    max_added_agents_per_day: 1,
                    max_total_agent_days: 105,
                    max_staff_cost_cents: 1_100_000,
                    max_final_backlog: 300,
                    service_capacity_band: BoundedCount { min: 8, max: 8 },
                },
                criteria: JointRiskScreenCriteria {
                    max_joint_violation_bps: 10_000,
                    min_joint_recovery_bps: 0,
                    min_sla_improvement_bps: 0,
                },
            }),
        };
        let empirical_denied = handle_decision_empirical_resampling(
            State(state.clone()),
            employee_headers.clone(),
            decision_body(&empirical_request("tenant-a", "private")),
        )
        .await;
        assert_eq!(empirical_denied.status(), axum::http::StatusCode::FORBIDDEN);
        let empirical_wrong_scope = handle_decision_empirical_resampling(
            State(state.clone()),
            admin_headers.clone(),
            decision_body(&empirical_request("tenant-a", "staff")),
        )
        .await;
        assert_eq!(
            empirical_wrong_scope.status(),
            axum::http::StatusCode::BAD_REQUEST
        );
        let empirical_ok = handle_decision_empirical_resampling(
            State(state.clone()),
            admin_headers.clone(),
            decision_body(&empirical_request("tenant-a", "private")),
        )
        .await;
        assert_eq!(empirical_ok.status(), axum::http::StatusCode::OK);
        let empirical_body = axum::body::to_bytes(empirical_ok.into_body(), usize::MAX)
            .await
            .unwrap();
        let empirical_json: serde_json::Value = serde_json::from_slice(&empirical_body).unwrap();
        assert_eq!(empirical_json["status"], "synthetic_only_exploratory");
        assert_eq!(
            empirical_json["fit"]["capacity_identification"],
            "empirical_saturated_days"
        );
        assert_eq!(empirical_json["run"]["runs"], 8);
        assert!(empirical_json["run"]["replay_hash"].as_str().unwrap().len() == 64);
        assert!(
            empirical_json["screen"]["replay_hash"]
                .as_str()
                .unwrap()
                .len()
                == 64
        );
        assert!(empirical_json.get("tickets").is_none());
        let shadow_scope = DecisionScope {
            tenant_id: "tenant-shadow-monitor".into(),
            acl: "private".into(),
        };
        let shadow_store = DecisionStore::with_causal_store(
            home.join("decisions.db"),
            CausalStore::new(home.join("memory.db")),
        );
        let shadow_start = chrono::Utc::now()
            .date_naive()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc()
            + chrono::Duration::days(2);
        let shadow_end = shadow_start + chrono::Duration::days(21);
        shadow_store
            .put_shadow_policy(
                &shadow_scope,
                "future-shadow-monitor-policy",
                "local-fixture-lineage",
                "support-queue",
                &shadow_start.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                &shadow_end.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                3_600,
                14,
                7,
            )
            .unwrap();
        let shadow_query =
            |tenant_id: &str, acl: &str, policy_id: &str| DecisionShadowMonitorQuery {
                tenant_id: tenant_id.into(),
                acl: acl.into(),
                policy_id: policy_id.into(),
            };
        let shadow_denied = handle_decision_shadow_monitor(
            State(state.clone()),
            employee_headers.clone(),
            decision_raw_query(&shadow_query(
                "tenant-shadow-monitor",
                "private",
                "future-shadow-monitor-policy",
            )),
        )
        .await;
        assert_eq!(shadow_denied.status(), axum::http::StatusCode::FORBIDDEN);
        let shadow_wrong_scope = handle_decision_shadow_monitor(
            State(state.clone()),
            admin_headers.clone(),
            decision_raw_query(&shadow_query(
                "tenant-shadow-monitor",
                "staff",
                "future-shadow-monitor-policy",
            )),
        )
        .await;
        assert_eq!(
            shadow_wrong_scope.status(),
            axum::http::StatusCode::NOT_FOUND
        );
        let shadow_before_count: i64 = rusqlite::Connection::open(home.join("decisions.db"))
            .unwrap()
            .query_row("SELECT COUNT(*) FROM decision_inputs", [], |row| row.get(0))
            .unwrap();
        let shadow_empty = handle_decision_shadow_monitor(
            State(state.clone()),
            admin_headers.clone(),
            decision_raw_query(&shadow_query(
                "tenant-shadow-monitor",
                "private",
                "future-shadow-monitor-policy",
            )),
        )
        .await;
        assert_eq!(shadow_empty.status(), axum::http::StatusCode::OK);
        let shadow_body = axum::body::to_bytes(shadow_empty.into_body(), 100_000)
            .await
            .unwrap();
        let shadow_json: serde_json::Value = serde_json::from_slice(&shadow_body).unwrap();
        assert_eq!(shadow_json["status"], "descriptive_shadow_monitor");
        assert_eq!(shadow_json["policy"]["queue_id"], "support-queue");
        assert_eq!(shadow_json["aggregate"]["due_days"], 0);
        assert_eq!(shadow_json["sla"]["due_days"], 0);
        assert_eq!(
            shadow_json["aggregate"]["whole_window_skill"],
            serde_json::Value::Null
        );
        assert_eq!(shadow_json["sla"]["drift_signal"], serde_json::Value::Null);
        assert!(!String::from_utf8_lossy(&shadow_body).contains("local-fixture-lineage"));
        let shadow_after_count: i64 = rusqlite::Connection::open(home.join("decisions.db"))
            .unwrap()
            .query_row("SELECT COUNT(*) FROM decision_inputs", [], |row| row.get(0))
            .unwrap();
        assert_eq!(shadow_before_count, shadow_after_count);
        let lifecycle_request =
            |tenant_id: &str, seed: u64| DecisionSyntheticPilotLifecycleRequest {
                tenant_id: tenant_id.into(),
                acl: "private".into(),
                seed,
                days: 35,
                kind: SyntheticLifecycleKind::Deleted,
            };
        let denied_lifecycle = handle_decision_synthetic_pilot_lifecycle(
            State(state.clone()),
            employee_headers.clone(),
            decision_body(&lifecycle_request("tenant-a", 47)),
        )
        .await;
        assert_eq!(denied_lifecycle.status(), axum::http::StatusCode::FORBIDDEN);
        let wrong_scope_lifecycle = handle_decision_synthetic_pilot_lifecycle(
            State(state.clone()),
            admin_headers.clone(),
            decision_body(&lifecycle_request("other-tenant", 47)),
        )
        .await;
        assert_eq!(
            wrong_scope_lifecycle.status(),
            axum::http::StatusCode::NOT_FOUND
        );
        let unbound_lifecycle = handle_decision_synthetic_pilot_lifecycle(
            State(state.clone()),
            admin_headers.clone(),
            decision_body(&lifecycle_request("tenant-a", 48)),
        )
        .await;
        assert_eq!(
            unbound_lifecycle.status(),
            axum::http::StatusCode::NOT_FOUND
        );
        let denied_validation = handle_decision_engineering_validation(
            State(state.clone()),
            employee_headers.clone(),
            decision_body(&DecisionEngineeringValidationRequest {
                tenant_id: "tenant-a".into(),
                acl: "private".into(),
                snapshot_id: "synthetic-support-47-35".into(),
                model_version: "dashboard-synthetic-capacity-v1-47-35".into(),
            }),
        )
        .await;
        assert_eq!(
            denied_validation.status(),
            axum::http::StatusCode::FORBIDDEN
        );
        let validated_demo = handle_decision_engineering_validation(
            State(state.clone()),
            admin_headers.clone(),
            decision_body(&DecisionEngineeringValidationRequest {
                tenant_id: "tenant-a".into(),
                acl: "private".into(),
                snapshot_id: "synthetic-support-47-35".into(),
                model_version: "dashboard-synthetic-capacity-v1-47-35".into(),
            }),
        )
        .await;
        assert_eq!(validated_demo.status(), axum::http::StatusCode::OK);
        let synthetic_forecast_request = DecisionForecastValidationRequest {
            tenant_id: "tenant-a".into(),
            acl: "private".into(),
            snapshot_id: "synthetic-support-47-35".into(),
            model_version: "dashboard-synthetic-capacity-v1-47-35".into(),
            baseline_scenario_id: "dashboard-synthetic-baseline-two-agents-47-35".into(),
            alternative_scenario_id: "dashboard-synthetic-three-agents-47-35".into(),
        };
        let synthetic_forecast = handle_decision_forecast_validation(
            State(state.clone()),
            admin_headers.clone(),
            decision_body(&synthetic_forecast_request),
        )
        .await;
        assert_eq!(synthetic_forecast.status(), axum::http::StatusCode::OK);
        let synthetic_forecast_body = axum::body::to_bytes(synthetic_forecast.into_body(), 100_000)
            .await
            .unwrap();
        let synthetic_forecast_json: serde_json::Value =
            serde_json::from_slice(&synthetic_forecast_body).unwrap();
        assert_eq!(
            synthetic_forecast_json["report"]["status"],
            "synthetic_only_exploratory"
        );
        assert_eq!(
            synthetic_forecast_json["report"]["forecast_evaluation_days"],
            21
        );
        assert!(
            synthetic_forecast_json["report"]["fixed_interval_evaluated_points"]
                .as_u64()
                .is_some()
        );
        // Decimal string, not a JSON number: see the operator-upload test above.
        assert!(
            synthetic_forecast_json["report"]["model_abs_error_sum"]
                .as_str()
                .is_some_and(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        );
        let catalog = handle_decision_catalog(
            State(state.clone()),
            admin_headers.clone(),
            decision_raw_query(&DecisionCatalogQuery {
                tenant_id: "tenant-a".into(),
                acl: "private".into(),
            }),
        )
        .await;
        assert_eq!(catalog.status(), axum::http::StatusCode::OK);
        let compared = handle_decision_compare(
            State(state.clone()),
            admin_headers.clone(),
            decision_body(&DecisionCompareRequest {
                tenant_id: "tenant-a".into(),
                acl: "private".into(),
                snapshot_id: "synthetic-support-47-35".into(),
                model_version: "dashboard-synthetic-capacity-v1-47-35".into(),
                baseline_scenario_id: "dashboard-synthetic-baseline-two-agents-47-35".into(),
                alternative_scenario_id: "dashboard-synthetic-three-agents-47-35".into(),
                empirical_run_id: None,
                policy_screen_hash: None,
                baseline_event_replay_hash: None,
                alternative_event_replay_hash: None,
                sla_holdout_id: None,
                forecast_validation_id: Some(
                    synthetic_forecast_json["report"]["record_id"]
                        .as_str()
                        .unwrap()
                        .into(),
                ),
                effect_ids: Vec::new(),
            }),
        )
        .await;
        assert_eq!(compared.status(), axum::http::StatusCode::OK);
        let event_request = |tenant_id: &str| DecisionEventCompareRequest {
            tenant_id: tenant_id.into(),
            acl: "private".into(),
            snapshot_id: "synthetic-support-47-35".into(),
            model_version: "dashboard-synthetic-capacity-v1-47-35".into(),
            baseline_scenario_id: "dashboard-synthetic-baseline-two-agents-47-35".into(),
            alternative_scenario_id: "dashboard-synthetic-three-agents-47-35".into(),
        };
        let event_denied = handle_decision_event_compare(
            State(state.clone()),
            employee_headers.clone(),
            decision_body(&event_request("tenant-a")),
        )
        .await;
        assert_eq!(event_denied.status(), axum::http::StatusCode::FORBIDDEN);
        let event_wrong_scope = handle_decision_event_compare(
            State(state.clone()),
            admin_headers.clone(),
            decision_body(&event_request("other-tenant")),
        )
        .await;
        assert_eq!(
            event_wrong_scope.status(),
            axum::http::StatusCode::NOT_FOUND
        );
        let event_compared = handle_decision_event_compare(
            State(state.clone()),
            admin_headers.clone(),
            decision_body(&event_request("tenant-a")),
        )
        .await;
        assert_eq!(event_compared.status(), axum::http::StatusCode::OK);
        let event_body = axum::body::to_bytes(event_compared.into_body(), 20_000)
            .await
            .unwrap();
        let event_json: serde_json::Value = serde_json::from_slice(&event_body).unwrap();
        assert_eq!(event_json["report"]["source_origin"], "synthetic");
        assert_eq!(
            event_json["report"]["baseline"]["replay_hash"]
                .as_str()
                .unwrap()
                .len(),
            64
        );
        assert!(event_json["report"]["baseline"].get("days").is_none());
        assert!(event_json["report"].get("tickets").is_none());
        let evidence_brief = handle_decision_compare(
            State(state.clone()),
            admin_headers.clone(),
            decision_body(&DecisionCompareRequest {
                tenant_id: "tenant-a".into(),
                acl: "private".into(),
                snapshot_id: "synthetic-support-47-35".into(),
                model_version: "dashboard-synthetic-capacity-v1-47-35".into(),
                baseline_scenario_id: "dashboard-synthetic-baseline-two-agents-47-35".into(),
                alternative_scenario_id: "dashboard-synthetic-three-agents-47-35".into(),
                empirical_run_id: Some(empirical_json["run"]["id"].as_str().unwrap().into()),
                policy_screen_hash: Some(
                    empirical_json["screen"]["replay_hash"]
                        .as_str()
                        .unwrap()
                        .into(),
                ),
                baseline_event_replay_hash: Some(
                    event_json["report"]["baseline"]["replay_hash"]
                        .as_str()
                        .unwrap()
                        .into(),
                ),
                alternative_event_replay_hash: Some(
                    event_json["report"]["alternative"]["replay_hash"]
                        .as_str()
                        .unwrap()
                        .into(),
                ),
                sla_holdout_id: None,
                forecast_validation_id: Some(
                    synthetic_forecast_json["report"]["record_id"]
                        .as_str()
                        .unwrap()
                        .into(),
                ),
                effect_ids: Vec::new(),
            }),
        )
        .await;
        assert_eq!(evidence_brief.status(), axum::http::StatusCode::OK);
        let evidence_body = axum::body::to_bytes(evidence_brief.into_body(), usize::MAX)
            .await
            .unwrap();
        let evidence_json: serde_json::Value = serde_json::from_slice(&evidence_body).unwrap();
        assert_eq!(
            evidence_json["brief"]["exploratory_empirical"]["run_id"],
            empirical_json["run"]["id"]
        );
        assert_eq!(
            evidence_json["brief"]["exploratory_policy_screen"]["screen_hash"],
            empirical_json["screen"]["replay_hash"]
        );
        assert_eq!(
            evidence_json["brief"]["exploratory_event"]["baseline_run_hash"],
            event_json["report"]["baseline"]["replay_hash"]
        );
        assert_eq!(
            evidence_json["brief"]["exploratory_event"]["alternative_run_hash"],
            event_json["report"]["alternative"]["replay_hash"]
        );
        assert_eq!(
            evidence_json["brief"]["exploratory_forecast"]["record_id"],
            synthetic_forecast_json["report"]["record_id"]
        );
        // Brief and report agree on one decimal-string wire type.
        assert_eq!(
            evidence_json["brief"]["exploratory_forecast"]["model_abs_error_sum"],
            synthetic_forecast_json["report"]["model_abs_error_sum"]
        );
        let event_brief_request = || DecisionCompareRequest {
            tenant_id: "tenant-a".into(),
            acl: "private".into(),
            snapshot_id: "synthetic-support-47-35".into(),
            model_version: "dashboard-synthetic-capacity-v1-47-35".into(),
            baseline_scenario_id: "dashboard-synthetic-baseline-two-agents-47-35".into(),
            alternative_scenario_id: "dashboard-synthetic-three-agents-47-35".into(),
            empirical_run_id: Some(empirical_json["run"]["id"].as_str().unwrap().into()),
            policy_screen_hash: None,
            baseline_event_replay_hash: Some(
                event_json["report"]["baseline"]["replay_hash"]
                    .as_str()
                    .unwrap()
                    .into(),
            ),
            alternative_event_replay_hash: Some(
                event_json["report"]["alternative"]["replay_hash"]
                    .as_str()
                    .unwrap()
                    .into(),
            ),
            sla_holdout_id: None,
            forecast_validation_id: None,
            effect_ids: Vec::new(),
        };
        let mut incomplete_events = event_brief_request();
        incomplete_events.alternative_event_replay_hash = None;
        assert_eq!(
            handle_decision_compare(
                State(state.clone()),
                admin_headers.clone(),
                decision_body(&incomplete_events),
            )
            .await
            .status(),
            axum::http::StatusCode::BAD_REQUEST
        );
        let mut no_empirical = event_brief_request();
        no_empirical.empirical_run_id = None;
        assert_eq!(
            handle_decision_compare(
                State(state.clone()),
                admin_headers.clone(),
                decision_body(&no_empirical),
            )
            .await
            .status(),
            axum::http::StatusCode::BAD_REQUEST
        );
        let mut wrong_hash = event_brief_request();
        wrong_hash.baseline_event_replay_hash = Some("0".repeat(64));
        assert_eq!(
            handle_decision_compare(
                State(state.clone()),
                admin_headers.clone(),
                decision_body(&wrong_hash),
            )
            .await
            .status(),
            axum::http::StatusCode::BAD_REQUEST
        );
        let mut wrong_pair = event_brief_request();
        wrong_pair.alternative_scenario_id = wrong_pair.baseline_scenario_id.clone();
        assert_ne!(
            handle_decision_compare(
                State(state.clone()),
                admin_headers.clone(),
                decision_body(&wrong_pair),
            )
            .await
            .status(),
            axum::http::StatusCode::OK
        );
        assert_eq!(
            handle_decision_compare(
                State(state.clone()),
                employee_headers.clone(),
                decision_body(&event_brief_request()),
            )
            .await
            .status(),
            axum::http::StatusCode::FORBIDDEN
        );
        assert!(
            serde_json::from_value::<DecisionCompareRequest>(serde_json::json!({
                "tenant_id": "tenant-a", "acl": "private", "snapshot_id": "synthetic-support-47-35",
                "model_version": "dashboard-synthetic-capacity-v1-47-35",
                "baseline_scenario_id": "dashboard-synthetic-baseline-two-agents-47-35",
                "alternative_scenario_id": "dashboard-synthetic-three-agents-47-35",
                "empirical_run_id": empirical_json["run"]["id"],
                "baseline_event_replay_hash": event_json["report"]["baseline"]["replay_hash"],
                "alternative_event_replay_hash": event_json["report"]["alternative"]["replay_hash"],
                "source_text": "private-ticket-row"
            }))
            .is_err()
        );
        let policy = handle_decision_policy_sweep(
            State(state.clone()),
            admin_headers.clone(),
            decision_body(&DecisionPolicySweepRequest {
                tenant_id: "tenant-a".into(),
                acl: "private".into(),
                snapshot_id: "synthetic-support-47-35".into(),
                model_version: "dashboard-synthetic-capacity-v1-47-35".into(),
                baseline_scenario_id: "dashboard-synthetic-baseline-two-agents-47-35".into(),
                alternative_scenario_id: "dashboard-synthetic-three-agents-47-35".into(),
                resource_plan: StaffingResourcePlan {
                    available_agents_by_day: vec![3; 35],
                    max_added_agents_per_day: 1,
                    max_total_agent_days: 105,
                    max_staff_cost_cents: 1_100_000,
                    max_final_backlog: 300,
                    service_capacity_band: crate::decision_sensitivity::BoundedCount {
                        min: 7,
                        max: 9,
                    },
                },
            }),
        )
        .await;
        assert_eq!(policy.status(), axum::http::StatusCode::OK);
        let sensitivity = handle_decision_sensitivity(
            State(state.clone()),
            admin_headers.clone(),
            decision_body(&DecisionSensitivityRequest {
                tenant_id: "tenant-a".into(),
                acl: "private".into(),
                snapshot_id: "synthetic-support-47-35".into(),
                model_version: "dashboard-synthetic-capacity-v1-47-35".into(),
                baseline_scenario_id: "dashboard-synthetic-baseline-two-agents-47-35".into(),
                alternative_scenario_id: "dashboard-synthetic-three-agents-47-35".into(),
                runs: 32,
                arrival_delta: 2,
                capacity_min: 7,
                capacity_max: 9,
                max_final_backlog: 300,
                max_staff_cost_cents: 1_100_000,
            }),
        )
        .await;
        assert_eq!(sensitivity.status(), axum::http::StatusCode::OK);
        let decision_store = DecisionStore::with_causal_store(
            home.join("decisions.db"),
            CausalStore::new(home.join("memory.db")),
        );
        let replay_hash = decision_store
            .replay(
                &DecisionScope {
                    tenant_id: "tenant-a".into(),
                    acl: "private".into(),
                },
                "synthetic-support-47-35",
                "dashboard-synthetic-capacity-v1-47-35",
                "dashboard-synthetic-baseline-two-agents-47-35",
            )
            .unwrap()
            .replay_hash;
        let replayed = handle_decision_replay(
            State(state.clone()),
            admin_headers.clone(),
            decision_body(&DecisionReplayRequest {
                tenant_id: "tenant-a".into(),
                acl: "private".into(),
                snapshot_id: "synthetic-support-47-35".into(),
                model_version: "dashboard-synthetic-capacity-v1-47-35".into(),
                scenario_id: "dashboard-synthetic-baseline-two-agents-47-35".into(),
                expected_hash: replay_hash.clone(),
            }),
        )
        .await;
        assert_eq!(replayed.status(), axum::http::StatusCode::OK);
        let stale_replay = handle_decision_replay(
            State(state.clone()),
            admin_headers.clone(),
            decision_body(&DecisionReplayRequest {
                tenant_id: "tenant-a".into(),
                acl: "private".into(),
                snapshot_id: "synthetic-support-47-35".into(),
                model_version: "dashboard-synthetic-capacity-v1-47-35".into(),
                scenario_id: "dashboard-synthetic-baseline-two-agents-47-35".into(),
                expected_hash: "wrong".into(),
            }),
        )
        .await;
        assert_eq!(stale_replay.status(), axum::http::StatusCode::CONFLICT);

        let review_request = || DecisionPilotReviewRequest {
            tenant_id: "tenant-a".into(),
            acl: "private".into(),
            snapshot_id: "synthetic-support-47-35".into(),
            model_version: "dashboard-synthetic-capacity-v1-47-35".into(),
            scenario_id: "dashboard-synthetic-baseline-two-agents-47-35".into(),
            expected_hash: replay_hash.clone(),
            summary: "Inspect source assumptions and daily backlog trajectory".into(),
            ttl_seconds: 3600,
        };
        let denied_review = handle_decision_pilot_review_request(
            State(state.clone()),
            employee_headers.clone(),
            decision_body(&review_request()),
        )
        .await;
        assert_eq!(denied_review.status(), axum::http::StatusCode::FORBIDDEN);
        let stale_review_request = handle_decision_pilot_review_request(
            State(state.clone()),
            admin_headers.clone(),
            decision_body(&DecisionPilotReviewRequest {
                expected_hash: "0".repeat(64),
                ..review_request()
            }),
        )
        .await;
        assert_eq!(
            stale_review_request.status(),
            axum::http::StatusCode::CONFLICT
        );
        let requested_review = handle_decision_pilot_review_request(
            State(state.clone()),
            admin_headers.clone(),
            decision_body(&review_request()),
        )
        .await;
        assert_eq!(requested_review.status(), axum::http::StatusCode::OK);
        let body = axum::body::to_bytes(requested_review.into_body(), 100_000)
            .await
            .unwrap();
        let review_json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(review_json["review"]["status"], "pending");
        assert_eq!(review_json["review"]["link"]["replay_hash"], replay_hash);
        let approval_id = review_json["review"]["link"]["approval_id"]
            .as_str()
            .unwrap()
            .to_owned();
        let status_request = |tenant_id: &str, hash: &str| DecisionPilotReviewStatusRequest {
            tenant_id: tenant_id.into(),
            acl: "private".into(),
            snapshot_id: "synthetic-support-47-35".into(),
            model_version: "dashboard-synthetic-capacity-v1-47-35".into(),
            scenario_id: "dashboard-synthetic-baseline-two-agents-47-35".into(),
            expected_hash: hash.into(),
            approval_id: approval_id.clone(),
        };
        let wrong_scope_review = handle_decision_pilot_review_status(
            State(state.clone()),
            admin_headers.clone(),
            decision_body(&status_request("other-tenant", &replay_hash)),
        )
        .await;
        assert_eq!(
            wrong_scope_review.status(),
            axum::http::StatusCode::NOT_FOUND
        );
        let wrong_hash_review = handle_decision_pilot_review_status(
            State(state.clone()),
            admin_headers.clone(),
            decision_body(&status_request("tenant-a", &"1".repeat(64))),
        )
        .await;
        assert_eq!(wrong_hash_review.status(), axum::http::StatusCode::CONFLICT);
        let denied_status = handle_decision_pilot_review_status(
            State(state.clone()),
            employee_headers.clone(),
            decision_body(&status_request("tenant-a", &replay_hash)),
        )
        .await;
        assert_eq!(denied_status.status(), axum::http::StatusCode::FORBIDDEN);
        let broker = crate::approval::ApprovalBroker::open(&home).unwrap();
        broker
            .decide(
                &crate::approval::ApprovalId::from(approval_id.clone()),
                true,
                "human-reviewer",
            )
            .await
            .unwrap();
        let approved_review = handle_decision_pilot_review_status(
            State(state.clone()),
            admin_headers.clone(),
            decision_body(&status_request("tenant-a", &replay_hash)),
        )
        .await;
        assert_eq!(approved_review.status(), axum::http::StatusCode::OK);
        let body = axum::body::to_bytes(approved_review.into_body(), 100_000)
            .await
            .unwrap();
        let approved_json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(approved_json["review"]["status"], "approved");
        assert_eq!(approved_json["review"]["decided_by"], "human-reviewer");
        let scope = DecisionScope {
            tenant_id: "tenant-a".into(),
            acl: "private".into(),
        };
        let source = decision_store
            .load_daily_run(&scope, &replay_hash)
            .unwrap()
            .source_version_hashes[0]
            .clone();
        decision_store
            .revoke_source_version(&scope, &source)
            .unwrap();
        let revoked_review = handle_decision_pilot_review_status(
            State(state.clone()),
            admin_headers.clone(),
            decision_body(&status_request("tenant-a", &replay_hash)),
        )
        .await;
        assert_eq!(revoked_review.status(), axum::http::StatusCode::GONE);
        let staged_lifecycle = handle_decision_synthetic_pilot_lifecycle(
            State(state.clone()),
            admin_headers.clone(),
            decision_body(&lifecycle_request("tenant-a", 47)),
        )
        .await;
        assert_eq!(staged_lifecycle.status(), axum::http::StatusCode::OK);
        let staged_body = axum::body::to_bytes(staged_lifecycle.into_body(), 10_000)
            .await
            .unwrap();
        let staged_json: serde_json::Value = serde_json::from_slice(&staged_body).unwrap();
        assert_eq!(staged_json["stage_outcome"], "staged");
        let connector_dashboard = handle_ccr_dashboard(
            State(state.clone()),
            admin_headers.clone(),
            Query(CcrDashboardQuery {
                tenant_id: "tenant-a".into(),
            }),
        )
        .await;
        assert_eq!(connector_dashboard.status(), axum::http::StatusCode::OK);
        let dashboard_body = axum::body::to_bytes(connector_dashboard.into_body(), 100_000)
            .await
            .unwrap();
        let dashboard_json: serde_json::Value = serde_json::from_slice(&dashboard_body).unwrap();
        assert_eq!(dashboard_json["connector_lifecycle"]["pending_events"], 1);
        let byte_check_pilot = handle_decision_synthetic_pilot(
            State(state.clone()),
            admin_headers.clone(),
            decision_body(&DecisionSyntheticPilotRequest {
                tenant_id: "tenant-byte-check".into(),
                acl: "private".into(),
                seed: 47,
                days: 35,
            }),
        )
        .await;
        assert_eq!(byte_check_pilot.status(), axum::http::StatusCode::OK);
        let byte_check_body = axum::body::to_bytes(byte_check_pilot.into_body(), 10_000)
            .await
            .unwrap();
        let byte_check_json: serde_json::Value = serde_json::from_slice(&byte_check_body).unwrap();
        let byte_check_artifact = byte_check_json["source_artifact_id"].as_str().unwrap();
        rusqlite::Connection::open(home.join("memory.db"))
            .unwrap()
            .execute(
                "UPDATE causal_artifacts SET content='changed' WHERE id=?1",
                [byte_check_artifact],
            )
            .unwrap();
        let byte_check_rejected = handle_decision_synthetic_pilot_lifecycle(
            State(state.clone()),
            admin_headers.clone(),
            decision_body(&lifecycle_request("tenant-byte-check", 47)),
        )
        .await;
        assert_eq!(
            byte_check_rejected.status(),
            axum::http::StatusCode::CONFLICT
        );

        let protocol_request = || CausalNegativeControlProtocolRequest {
            tenant_id: "tenant-a".into(),
            acl: "private".into(),
            external_id: "exclusion".into(),
            version: "v1".into(),
            lineage_id: "synthetic-exclusion".into(),
            content: "Synthetic exclusion rule for a negative-control outcome".into(),
            occurred_at: 1,
            retention_at: i64::MAX,
        };
        let protocol_denied = handle_causal_negative_control_protocol(
            State(state.clone()),
            employee_headers.clone(),
            Json(protocol_request()),
        )
        .await;
        assert_eq!(protocol_denied.status(), axum::http::StatusCode::FORBIDDEN);
        let protocol_created = handle_causal_negative_control_protocol(
            State(state.clone()),
            admin_headers.clone(),
            Json(protocol_request()),
        )
        .await;
        assert_eq!(protocol_created.status(), axum::http::StatusCode::OK);
        let store = CausalStore::new(home.join("memory.db"));
        let expected_protocol = store
            .add_artifact(
                &EvidenceScope {
                    tenant_id: "tenant-a".into(),
                    acl: "private".into(),
                },
                "negative_control_protocol",
                "exclusion",
                "v1",
                "synthetic-exclusion",
                "Synthetic exclusion rule for a negative-control outcome",
                1,
                i64::MAX,
            )
            .unwrap();
        assert!(!expected_protocol.id.is_empty());
        let review_denied = handle_causal_negative_control_review(
            State(state.clone()),
            employee_headers.clone(),
            Json(CausalNegativeControlReviewRequest {
                tenant_id: "tenant-a".into(),
                acl: "private".into(),
                model_id: "missing".into(),
                variable_id: "missing".into(),
                protocol_artifact_id: expected_protocol.id.clone(),
                verdict: duduclaw_memory::causal_model::AssumptionVerdict::Pass,
                rationale: "This review must not be accepted from an employee".into(),
                expected_review_id: None,
            }),
        )
        .await;
        assert_eq!(review_denied.status(), axum::http::StatusCode::FORBIDDEN);

        let denied_extract = handle_causal_extract(
            State(state.clone()),
            employee_headers.clone(),
            Json(CausalExtractRequest {
                tenant_id: "tenant-a".into(),
                acl: "private".into(),
                artifact_id: "source".into(),
                question: "Why?".into(),
            }),
        )
        .await;
        assert_eq!(denied_extract.status(), axum::http::StatusCode::FORBIDDEN);
        let disabled_extract = handle_causal_extract(
            State(state.clone()),
            admin_headers.clone(),
            Json(CausalExtractRequest {
                tenant_id: "tenant-a".into(),
                acl: "private".into(),
                artifact_id: "source".into(),
                question: "Why?".into(),
            }),
        )
        .await;
        assert_eq!(
            disabled_extract.status(),
            axum::http::StatusCode::SERVICE_UNAVAILABLE
        );
        std::fs::write(
            home.join("config.toml"),
            "[causal_extraction]\nenabled = true\nprovider = 'anthropic'\nmodel = 'claude-test'\nallowed_scopes = [{tenant_id='different-tenant',acl='private'}]",
        )
        .unwrap();
        let denied_scope_extract = handle_causal_extract(
            State(state.clone()),
            admin_headers.clone(),
            Json(CausalExtractRequest {
                tenant_id: "tenant-a".into(),
                acl: "private".into(),
                artifact_id: "source".into(),
                question: "Why?".into(),
            }),
        )
        .await;
        assert_eq!(
            denied_scope_extract.status(),
            axum::http::StatusCode::FORBIDDEN
        );

        let conn = rusqlite::Connection::open(home.join("memory.db")).unwrap();
        conn.execute_batch(
            "CREATE TABLE memories (
            id TEXT PRIMARY KEY, agent_id TEXT NOT NULL, content TEXT NOT NULL,
            timestamp TEXT NOT NULL, quarantined INTEGER NOT NULL DEFAULT 0,
            valid_until TEXT, invalidated_at TEXT
        );
        INSERT INTO memories (id,agent_id,content,timestamp)
        VALUES ('m1','agent-a','synthetic note','2026-01-01T00:00:00Z');",
        )
        .unwrap();
        drop(conn);
        let denied_import = handle_causal_import_memory(
            State(state.clone()),
            employee_headers.clone(),
            Json(CausalImportMemoryRequest {
                agent_id: "agent-a".into(),
                memory_id: "m1".into(),
            }),
        )
        .await;
        assert_eq!(denied_import.status(), axum::http::StatusCode::FORBIDDEN);
        let imported = handle_causal_import_memory(
            State(state.clone()),
            admin_headers.clone(),
            Json(CausalImportMemoryRequest {
                agent_id: "agent-a".into(),
                memory_id: "m1".into(),
            }),
        )
        .await;
        assert_eq!(imported.status(), axum::http::StatusCode::OK);
        let body = axum::body::to_bytes(imported.into_body(), 100_000)
            .await
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["source"]["acl"], "agent-private");
        assert_eq!(value["source"]["tenant_id"], "agent-a");
        let wiki = home.join("agents/agent-a/wiki/sources");
        std::fs::create_dir_all(&wiki).unwrap();
        std::fs::write(wiki.join("queue.md"),
            "---\ntitle: Queue\ncreated: 2026-01-01\nupdated: 2026-01-02\ntrust: 0.9\n---\nSynthetic queue note.\n").unwrap();
        let denied_wiki = handle_causal_import_wiki(
            State(state.clone()),
            employee_headers.clone(),
            Json(CausalImportWikiRequest {
                agent_id: "agent-a".into(),
                page_path: "sources/queue.md".into(),
            }),
        )
        .await;
        assert_eq!(denied_wiki.status(), axum::http::StatusCode::FORBIDDEN);
        let imported_wiki = handle_causal_import_wiki(
            State(state.clone()),
            admin_headers.clone(),
            Json(CausalImportWikiRequest {
                agent_id: "agent-a".into(),
                page_path: "sources/queue.md".into(),
            }),
        )
        .await;
        assert_eq!(imported_wiki.status(), axum::http::StatusCode::OK);
        let body = axum::body::to_bytes(imported_wiki.into_body(), 100_000)
            .await
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["source"]["kind"], "wiki_agent");
        assert_eq!(value["source"]["acl"], "agent-private");
        let shared = home.join("shared/wiki/sources");
        std::fs::create_dir_all(&shared).unwrap();
        std::fs::write(shared.join("queue.md"),
            "---\ntitle: Shared queue\ncreated: 2026-01-01\nupdated: 2026-01-02\ntrust: 0.9\n---\nShared synthetic note.\n").unwrap();
        std::fs::write(
            home.join("shared/wiki/.scope.toml"),
            "[namespaces.sources]\nmode = 'agent_writable'\n",
        )
        .unwrap();
        let denied_shared = handle_causal_import_shared_wiki(
            State(state.clone()),
            employee_headers.clone(),
            Json(CausalImportSharedWikiRequest {
                page_path: "sources/queue.md".into(),
            }),
        )
        .await;
        assert_eq!(denied_shared.status(), axum::http::StatusCode::FORBIDDEN);
        let imported_shared = handle_causal_import_shared_wiki(
            State(state.clone()),
            admin_headers.clone(),
            Json(CausalImportSharedWikiRequest {
                page_path: "sources/queue.md".into(),
            }),
        )
        .await;
        assert_eq!(imported_shared.status(), axum::http::StatusCode::OK);
        let body = axum::body::to_bytes(imported_shared.into_body(), 100_000)
            .await
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["source"]["kind"], "wiki_shared");
        assert_eq!(value["source"]["tenant_id"], "workspace");
        assert_eq!(value["source"]["acl"], "shared-wiki");

        let store = CausalStore::new(home.join("memory.db"));
        let scope = EvidenceScope {
            tenant_id: "tenant-a".into(),
            acl: "private".into(),
        };
        let source = store
            .add_artifact(
                &scope,
                "ticket",
                "t1",
                "v1",
                "thread-1",
                "A affects B",
                1,
                i64::MAX,
            )
            .unwrap();
        let claim = store
            .add_claim(
                &scope,
                "A",
                "B",
                0,
                1,
                &serde_json::json!({}),
                ClaimModality::Asserted,
            )
            .unwrap();
        store
            .add_evidence(
                &scope,
                &claim.id,
                &source.id,
                0,
                1,
                "A",
                EvidenceStance::Supports,
                None,
                "test",
            )
            .unwrap();
        let review = CausalClaimReviewRequest {
            tenant_id: scope.tenant_id.clone(),
            acl: scope.acl.clone(),
            id: claim.id.clone(),
            expected_state: "candidate".into(),
            accept: true,
        };
        let accepted =
            handle_causal_claim_review(State(state.clone()), admin_headers.clone(), Json(review))
                .await;
        assert_eq!(accepted.status(), axum::http::StatusCode::OK);
        assert_eq!(
            store
                .read_claim(&scope, &claim.id)
                .unwrap()
                .reviewer
                .as_deref(),
            Some(admin.id.as_str())
        );
        let stale = handle_causal_claim_review(
            State(state.clone()),
            admin_headers.clone(),
            Json(CausalClaimReviewRequest {
                tenant_id: scope.tenant_id.clone(),
                acl: scope.acl.clone(),
                id: claim.id.clone(),
                expected_state: "candidate".into(),
                accept: false,
            }),
        )
        .await;
        assert_eq!(stale.status(), axum::http::StatusCode::CONFLICT);
        let a = store
            .register_variable(
                &scope,
                "A",
                "v1",
                "candidate treatment",
                "yes/no",
                VariableKind::Binary,
            )
            .unwrap();
        let b = store
            .register_variable(
                &scope,
                "B",
                "v1",
                "candidate outcome",
                "count",
                VariableKind::Count,
            )
            .unwrap();
        let control = store
            .register_variable(
                &scope,
                "unrelated-control",
                "v1",
                "synthetic negative outcome",
                "count",
                VariableKind::Count,
            )
            .unwrap();
        let alias_set = handle_causal_alias_set(
            State(state.clone()),
            admin_headers.clone(),
            Json(CausalAliasSetRequest {
                tenant_id: scope.tenant_id.clone(),
                acl: scope.acl.clone(),
                alias: "Alt-A".into(),
                canonical_name: "A".into(),
                expected_review_id: None,
            }),
        )
        .await;
        assert_eq!(alias_set.status(), axum::http::StatusCode::OK);
        let body = axum::body::to_bytes(alias_set.into_body(), 100_000)
            .await
            .unwrap();
        let alias_response: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let alias_review_id = alias_response["alias"]["review_id"]
            .as_str()
            .unwrap()
            .to_owned();
        assert_eq!(alias_response["alias"]["reviewer"], admin.id);
        let aliases = handle_causal_aliases(
            State(state.clone()),
            admin_headers.clone(),
            Query(CausalAliasesQuery {
                tenant_id: scope.tenant_id.clone(),
                acl: scope.acl.clone(),
                limit: Some(10),
            }),
        )
        .await;
        let body = axum::body::to_bytes(aliases.into_body(), 100_000)
            .await
            .unwrap();
        let listed_aliases: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(listed_aliases["aliases"][0]["canonical_name"], "A");
        let stale_alias = handle_causal_alias_set(
            State(state.clone()),
            admin_headers.clone(),
            Json(CausalAliasSetRequest {
                tenant_id: scope.tenant_id.clone(),
                acl: scope.acl.clone(),
                alias: "Alt-A".into(),
                canonical_name: "A".into(),
                expected_review_id: None,
            }),
        )
        .await;
        assert_eq!(stale_alias.status(), axum::http::StatusCode::CONFLICT);
        let stale_revoke = handle_causal_alias_revoke(
            State(state.clone()),
            admin_headers.clone(),
            Json(CausalAliasRevokeRequest {
                tenant_id: scope.tenant_id.clone(),
                acl: scope.acl.clone(),
                alias: "Alt-A".into(),
                expected_review_id: "wrong".into(),
            }),
        )
        .await;
        assert_eq!(stale_revoke.status(), axum::http::StatusCode::CONFLICT);
        let revoked_alias = handle_causal_alias_revoke(
            State(state.clone()),
            admin_headers.clone(),
            Json(CausalAliasRevokeRequest {
                tenant_id: scope.tenant_id.clone(),
                acl: scope.acl.clone(),
                alias: "Alt-A".into(),
                expected_review_id: alias_review_id,
            }),
        )
        .await;
        assert_eq!(revoked_alias.status(), axum::http::StatusCode::OK);
        let model = store
            .create_model(
                &scope,
                &ModelDraft {
                    name: "review-pilot".into(),
                    version: "v1".into(),
                    treatment_variable_id: a.id.clone(),
                    outcome_variable_id: b.id.clone(),
                    population: "synthetic".into(),
                    window_start: 0,
                    window_end: 100,
                    variable_ids: vec![a.id, b.id, control.id.clone()],
                    claim_ids: vec![claim.id.clone()],
                },
            )
            .unwrap();
        let draft = handle_causal_model(
            State(state.clone()),
            admin_headers.clone(),
            Query(CausalItemQuery {
                tenant_id: scope.tenant_id.clone(),
                acl: scope.acl.clone(),
                id: model.id.clone(),
                offset: None,
                limit: None,
            }),
        )
        .await;
        assert_eq!(draft.status(), axum::http::StatusCode::OK);
        let body = axum::body::to_bytes(draft.into_body(), 100_000)
            .await
            .unwrap();
        let view: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(view["review"]["variables"].as_array().unwrap().len(), 3);
        assert_eq!(view["review"]["edges"][0]["id"], claim.id);
        assert_eq!(view["parent_adjustment"]["status"], "unknown");
        let original_digest = view["review"]["active_opposition_digest"]
            .as_str()
            .unwrap()
            .to_owned();
        let approved = handle_causal_model_review(
            State(state.clone()),
            admin_headers.clone(),
            Json(CausalModelReviewRequest {
                tenant_id: scope.tenant_id.clone(),
                acl: scope.acl.clone(),
                id: model.id.clone(),
                expected_state: "draft".into(),
                expected_opposition_digest: original_digest.clone(),
                approve: true,
                acknowledge_conflicts: false,
            }),
        )
        .await;
        assert_eq!(approved.status(), axum::http::StatusCode::OK);
        let approved_detail = handle_causal_model(
            State(state.clone()),
            admin_headers.clone(),
            Query(CausalItemQuery {
                tenant_id: scope.tenant_id.clone(),
                acl: scope.acl.clone(),
                id: model.id.clone(),
                offset: None,
                limit: None,
            }),
        )
        .await;
        assert_eq!(approved_detail.status(), axum::http::StatusCode::OK);
        let approved_body = axum::body::to_bytes(approved_detail.into_body(), 100_000)
            .await
            .unwrap();
        let approved_view: serde_json::Value = serde_json::from_slice(&approved_body).unwrap();
        assert_eq!(
            approved_view["parent_adjustment"]["status"],
            "graphically_admissible"
        );
        assert_eq!(
            approved_view["parent_adjustment"]["adjustment_variable_ids"],
            serde_json::json!([])
        );
        assert_eq!(
            store
                .read_model(&scope, &model.id)
                .unwrap()
                .reviewer
                .as_deref(),
            Some(admin.id.as_str())
        );
        let assumption_view = handle_causal_assumptions(
            State(state.clone()),
            admin_headers.clone(),
            Query(CausalAssumptionsQuery {
                tenant_id: scope.tenant_id.clone(),
                acl: scope.acl.clone(),
                model_id: model.id.clone(),
            }),
        )
        .await;
        assert_eq!(assumption_view.status(), axum::http::StatusCode::OK);
        let body = axum::body::to_bytes(assumption_view.into_body(), 100_000)
            .await
            .unwrap();
        let assumption_view: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(assumption_view["reviews"].as_array().unwrap().len(), 0);
        assert_eq!(assumption_view["readiness"]["state"], "unknown");
        let assumption_request = || CausalAssumptionReviewRequest {
            tenant_id: scope.tenant_id.clone(),
            acl: scope.acl.clone(),
            model_id: model.id.clone(),
            kind: duduclaw_memory::causal_model::AssumptionKind::DataAvailability,
            verdict: duduclaw_memory::causal_model::AssumptionVerdict::Pass,
            rationale: "Synthetic data availability has been reviewed".into(),
            expected_review_id: None,
        };
        let assumption_denied = handle_causal_assumption_review(
            State(state.clone()),
            employee_headers.clone(),
            Json(assumption_request()),
        )
        .await;
        assert_eq!(
            assumption_denied.status(),
            axum::http::StatusCode::FORBIDDEN
        );
        let assumption_saved = handle_causal_assumption_review(
            State(state.clone()),
            admin_headers.clone(),
            Json(assumption_request()),
        )
        .await;
        assert_eq!(assumption_saved.status(), axum::http::StatusCode::OK);
        let current_assumptions = store.current_assumption_reviews(&scope, &model.id).unwrap();
        assert_eq!(current_assumptions.len(), 1);
        assert_eq!(current_assumptions[0].reviewer, admin.id);
        let assumption_stale = handle_causal_assumption_review(
            State(state.clone()),
            admin_headers.clone(),
            Json(assumption_request()),
        )
        .await;
        assert_eq!(assumption_stale.status(), axum::http::StatusCode::CONFLICT);
        let reviewed_control = handle_causal_negative_control_review(
            State(state.clone()),
            admin_headers.clone(),
            Json(CausalNegativeControlReviewRequest {
                tenant_id: scope.tenant_id.clone(),
                acl: scope.acl.clone(),
                model_id: model.id.clone(),
                variable_id: control.id.clone(),
                protocol_artifact_id: expected_protocol.id.clone(),
                verdict: duduclaw_memory::causal_model::AssumptionVerdict::Pass,
                rationale: "Synthetic control is excluded from the treatment path".into(),
                expected_review_id: None,
            }),
        )
        .await;
        assert_eq!(reviewed_control.status(), axum::http::StatusCode::OK);
        let body = axum::body::to_bytes(reviewed_control.into_body(), 100_000)
            .await
            .unwrap();
        let review_response: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let review_id = review_response["review_id"].as_str().unwrap();
        let stale_control_review = handle_causal_negative_control_review(
            State(state.clone()),
            admin_headers.clone(),
            Json(CausalNegativeControlReviewRequest {
                tenant_id: scope.tenant_id.clone(),
                acl: scope.acl.clone(),
                model_id: model.id.clone(),
                variable_id: control.id.clone(),
                protocol_artifact_id: expected_protocol.id.clone(),
                verdict: duduclaw_memory::causal_model::AssumptionVerdict::Fail,
                rationale: "This stale submission must not replace a newer review".into(),
                expected_review_id: None,
            }),
        )
        .await;
        assert_eq!(
            stale_control_review.status(),
            axum::http::StatusCode::CONFLICT
        );
        assert!(matches!(
            store
                .negative_control_readiness(&scope, &model.id, &control.id, review_id)
                .unwrap(),
            duduclaw_memory::causal_negative_control::NegativeControlReadiness::Reviewed { .. }
        ));
        let conn = rusqlite::Connection::open(home.join("memory.db")).unwrap();
        let recorded_reviewer: String = conn
            .query_row(
                "SELECT reviewer FROM causal_negative_control_reviews WHERE id=?1",
                [review_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(recorded_reviewer, admin.id);
        let control_view = handle_causal_negative_control_review_get(
            State(state.clone()),
            admin_headers.clone(),
            Query(CausalNegativeControlReviewQuery {
                tenant_id: scope.tenant_id.clone(),
                acl: scope.acl.clone(),
                model_id: model.id.clone(),
                variable_id: control.id.clone(),
            }),
        )
        .await;
        assert_eq!(control_view.status(), axum::http::StatusCode::OK);
        let body = axum::body::to_bytes(control_view.into_body(), 100_000)
            .await
            .unwrap();
        let control_view: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(control_view["review"]["id"], review_id);
        assert_eq!(control_view["readiness"]["state"], "reviewed");
        let estimated = handle_causal_effect_estimate(
            State(state.clone()),
            admin_headers.clone(),
            Json(CausalEffectEstimateRequest {
                tenant_id: scope.tenant_id.clone(),
                acl: scope.acl.clone(),
                model_id: model.id.clone(),
                external_id: "empty-data".into(),
                version: "v1".into(),
                lineage_id: "empty-data".into(),
                occurred_at: 1,
                retention_at: i64::MAX,
                dataset: duduclaw_memory::causal_effect::ObservedDataset {
                    adjustment_variable_ids: vec![],
                    evaluation_cutoff: None,
                    negative_control: None,
                    units: vec![],
                },
            }),
        )
        .await;
        assert_eq!(estimated.status(), axum::http::StatusCode::OK);
        let body = axum::body::to_bytes(estimated.into_body(), 100_000)
            .await
            .unwrap();
        let effect_response: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(effect_response["result"]["state"], "unknown");
        assert!(effect_response["dataset_artifact_id"].as_str().is_some());
        let opposing = store
            .add_artifact(
                &scope,
                "ticket",
                "t2",
                "v1",
                "thread-2",
                "A may not affect B",
                2,
                i64::MAX,
            )
            .unwrap();
        store
            .add_evidence(
                &scope,
                &claim.id,
                &opposing.id,
                0,
                1,
                "A",
                EvidenceStance::Opposes,
                None,
                "test",
            )
            .unwrap();
        let stale_opposition = handle_causal_model_review(
            State(state.clone()),
            admin_headers.clone(),
            Json(CausalModelReviewRequest {
                tenant_id: scope.tenant_id.clone(),
                acl: scope.acl.clone(),
                id: model.id.clone(),
                expected_state: "approved".into(),
                expected_opposition_digest: original_digest,
                approve: true,
                acknowledge_conflicts: true,
            }),
        )
        .await;
        assert_eq!(stale_opposition.status(), axum::http::StatusCode::CONFLICT);
        let current = store.model_review_view(&scope, &model.id).unwrap();
        assert_eq!(current.effective_state, "needs_review");
        let reviewed = handle_causal_model_review(
            State(state.clone()),
            admin_headers.clone(),
            Json(CausalModelReviewRequest {
                tenant_id: scope.tenant_id.clone(),
                acl: scope.acl.clone(),
                id: model.id.clone(),
                expected_state: "approved".into(),
                expected_opposition_digest: current.active_opposition_digest,
                approve: true,
                acknowledge_conflicts: true,
            }),
        )
        .await;
        assert_eq!(reviewed.status(), axum::http::StatusCode::OK);
        let models = handle_causal_models(
            State(state.clone()),
            admin_headers.clone(),
            Query(CausalModelsQuery {
                tenant_id: scope.tenant_id.clone(),
                acl: scope.acl.clone(),
                limit: Some(10),
            }),
        )
        .await;
        let body = axum::body::to_bytes(models.into_body(), 100_000)
            .await
            .unwrap();
        let listed_models: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(listed_models["models"][0]["model"]["id"], model.id);
        let listed = handle_causal_claims(
            State(state.clone()),
            admin_headers.clone(),
            Query(CausalClaimsQuery {
                tenant_id: scope.tenant_id.clone(),
                acl: scope.acl.clone(),
                review_state: Some("accepted".into()),
                limit: Some(10),
            }),
        )
        .await;
        assert_eq!(listed.status(), axum::http::StatusCode::OK);
        let body = axum::body::to_bytes(listed.into_body(), 100_000)
            .await
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["claims"][0]["id"], claim.id);
        let detail = handle_causal_claim(
            State(state.clone()),
            admin_headers.clone(),
            Query(CausalItemQuery {
                tenant_id: scope.tenant_id.clone(),
                acl: scope.acl.clone(),
                id: claim.id.clone(),
                offset: None,
                limit: None,
            }),
        )
        .await;
        let body = axum::body::to_bytes(detail.into_body(), 100_000)
            .await
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["effective_state"], "accepted");
        assert_eq!(value["evidence"][0]["span"]["excerpt"], "A");
        let source_response = handle_causal_source(
            State(state.clone()),
            admin_headers.clone(),
            Query(CausalItemQuery {
                tenant_id: scope.tenant_id.clone(),
                acl: scope.acl.clone(),
                id: source.id.clone(),
                offset: None,
                limit: None,
            }),
        )
        .await;
        assert_eq!(source_response.status(), axum::http::StatusCode::OK);
        let body = axum::body::to_bytes(source_response.into_body(), 100_000)
            .await
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["text"], "A affects B");
        let evidence_id = store.evidence_for_claim(&scope, &claim.id).unwrap()[0]
            .span
            .id
            .clone();
        let revision_request = CausalClaimReviseRequest {
            tenant_id: scope.tenant_id.clone(),
            acl: scope.acl.clone(),
            id: claim.id.clone(),
            revision: duduclaw_memory::causal_revision::ClaimRevisionInput {
                expected_state: "accepted".into(),
                cause_variable: "A".into(),
                effect_variable: "C".into(),
                lag_min_seconds: 0,
                lag_max_seconds: 2,
                modality: ClaimModality::Speculated,
                context: serde_json::json!({}),
                evidence_ids: vec![evidence_id],
                note: "corrected candidate".into(),
            },
        };
        let denied_revision = handle_causal_claim_revise(
            State(state.clone()),
            employee_headers.clone(),
            Json(CausalClaimReviseRequest {
                tenant_id: revision_request.tenant_id.clone(),
                acl: revision_request.acl.clone(),
                id: revision_request.id.clone(),
                revision: revision_request.revision.clone(),
            }),
        )
        .await;
        assert_eq!(denied_revision.status(), axum::http::StatusCode::FORBIDDEN);
        let revised = handle_causal_claim_revise(
            State(state.clone()),
            admin_headers.clone(),
            Json(revision_request),
        )
        .await;
        assert_eq!(revised.status(), axum::http::StatusCode::OK);
        assert_eq!(
            store.read_claim(&scope, &claim.id).unwrap().review_state,
            "superseded"
        );
        let remove_request = || CausalSourceRemovalRequest {
            tenant_id: scope.tenant_id.clone(),
            acl: scope.acl.clone(),
            artifact_id: source.id.clone(),
        };
        let denied_remove = handle_causal_source_invalidate(
            State(state.clone()),
            employee_headers.clone(),
            Json(remove_request()),
        )
        .await;
        assert_eq!(denied_remove.status(), axum::http::StatusCode::FORBIDDEN);
        let wrong_scope_remove = handle_causal_source_invalidate(
            State(state.clone()),
            admin_headers.clone(),
            Json(CausalSourceRemovalRequest {
                tenant_id: "tenant-b".into(),
                ..remove_request()
            }),
        )
        .await;
        assert_eq!(
            wrong_scope_remove.status(),
            axum::http::StatusCode::NOT_FOUND
        );
        assert!(store.source_text(&scope, &source.id).is_ok());
        let invalidated = handle_causal_source_invalidate(
            State(state.clone()),
            admin_headers.clone(),
            Json(remove_request()),
        )
        .await;
        assert_eq!(invalidated.status(), axum::http::StatusCode::OK);
        let erased = handle_causal_source_erase(
            State(state.clone()),
            admin_headers.clone(),
            Json(remove_request()),
        )
        .await;
        assert_eq!(erased.status(), axum::http::StatusCode::OK);
        let erase_retry = handle_causal_source_erase(
            State(state.clone()),
            admin_headers.clone(),
            Json(remove_request()),
        )
        .await;
        assert_eq!(erase_retry.status(), axum::http::StatusCode::OK);
        let stored_content: String = rusqlite::Connection::open(home.join("memory.db"))
            .unwrap()
            .query_row(
                "SELECT content FROM causal_artifacts WHERE id=?1",
                [&source.id],
                |row| row.get(0),
            )
            .unwrap();
        assert!(stored_content.is_empty());
        let revoked = handle_causal_source(
            State(state.clone()),
            admin_headers.clone(),
            Query(CausalItemQuery {
                tenant_id: scope.tenant_id.clone(),
                acl: scope.acl.clone(),
                id: source.id.clone(),
                offset: None,
                limit: None,
            }),
        )
        .await;
        assert_eq!(revoked.status(), axum::http::StatusCode::NOT_FOUND);
        let detail = handle_causal_claim(
            State(state),
            admin_headers,
            Query(CausalItemQuery {
                tenant_id: scope.tenant_id,
                acl: scope.acl,
                id: claim.id,
                offset: None,
                limit: None,
            }),
        )
        .await;
        let body = axum::body::to_bytes(detail.into_body(), 100_000)
            .await
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["effective_state"], "superseded");
        assert_eq!(value["revisions"][0]["reviewer"], admin.id);
        assert_eq!(value["evidence"][0]["span"]["excerpt"], "");
        assert_eq!(
            store
                .model_state(
                    &EvidenceScope {
                        tenant_id: "tenant-a".into(),
                        acl: "private".into()
                    },
                    &model.id
                )
                .unwrap(),
            "needs_review"
        );
    }

    /// W3-2 regression: `begin_ccr_revocation`'s fence had no clear path, so
    /// an abandoned revoke hid a live source with no way back. The recovery
    /// route must stay admin-only, audited, and refuse once the revocation
    /// has actually taken effect.
    #[tokio::test]
    async fn clear_revocation_fence_is_admin_only_audited_and_refuses_a_real_revocation() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();
        let user_db = Arc::new(UserDb::new(&home.join("users.db")).unwrap());
        let admin = user_db
            .create_user(
                "fence-admin@example.test",
                "Admin",
                "test-password",
                UserRole::Admin,
            )
            .unwrap();
        let employee = user_db
            .create_user(
                "fence-employee@example.test",
                "Employee",
                "test-password",
                UserRole::Employee,
            )
            .unwrap();
        let jwt_config = Arc::new(JwtConfig::new(b"fence-clear-test-secret-32-bytes!!!!"));
        let admin_token = jwt_config.issue_access_token(&admin, &[]).unwrap();
        let employee_token = jwt_config.issue_access_token(&employee, &[]).unwrap();
        let (tx, _) = broadcast::channel(4);
        let (event_tx, _) = broadcast::channel(4);
        let state = Arc::new(AppState {
            auth: AuthManager::new(None),
            handler: MethodHandler::new(home.clone()).await,
            tx,
            event_tx,
            user_db,
            jwt_config,
            otp_delivery: Arc::new(crate::otp_delivery::ConfigOtpDeliverer::new(
                home.clone(),
                reqwest::Client::new(),
            )),
            home_dir: home.clone(),
        });
        let mut admin_headers = axum::http::HeaderMap::new();
        admin_headers.insert(
            "authorization",
            format!("Bearer {admin_token}").parse().unwrap(),
        );
        admin_headers.insert("origin", "http://localhost:18789".parse().unwrap());
        let mut employee_headers = admin_headers.clone();
        employee_headers.insert(
            "authorization",
            format!("Bearer {employee_token}").parse().unwrap(),
        );

        let store = CausalStore::new(home.join("memory.db"));
        let scope = EvidenceScope {
            tenant_id: "tenant-a".into(),
            acl: "private".into(),
        };
        let source = store
            .add_artifact(
                &scope,
                "ticket",
                "fence-api",
                "v1",
                "lineage",
                "staffing lowered backlog",
                1,
                chrono::Utc::now().timestamp() + 3600,
            )
            .unwrap();
        store.begin_ccr_revocation(&scope, &source.id).unwrap();
        let request = || CausalSourceRemovalRequest {
            tenant_id: scope.tenant_id.clone(),
            acl: scope.acl.clone(),
            artifact_id: source.id.clone(),
        };

        let denied = handle_causal_clear_revocation_fence(
            State(state.clone()),
            employee_headers,
            Json(request()),
        )
        .await;
        assert_eq!(denied.status(), axum::http::StatusCode::FORBIDDEN);
        let mut wrong_origin = admin_headers.clone();
        wrong_origin.insert("origin", "https://untrusted.example".parse().unwrap());
        let denied_origin = handle_causal_clear_revocation_fence(
            State(state.clone()),
            wrong_origin,
            Json(request()),
        )
        .await;
        assert_eq!(denied_origin.status(), axum::http::StatusCode::FORBIDDEN);

        let cleared = handle_causal_clear_revocation_fence(
            State(state.clone()),
            admin_headers.clone(),
            Json(request()),
        )
        .await;
        assert_eq!(cleared.status(), axum::http::StatusCode::OK);
        let body = axum::body::to_bytes(cleared.into_body(), 10_000)
            .await
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["cleared_version"], source.version);
        assert_eq!(
            store.source_text(&scope, &source.id).unwrap(),
            "staffing lowered backlog"
        );
        let audit = std::fs::read_to_string(home.join("security_audit.jsonl")).unwrap();
        assert!(
            audit.contains("causal_clear_revocation_fence"),
            "restoring a hidden source must leave an audit row"
        );

        // A revocation that took effect is not reversible here.
        store.invalidate_artifact(&scope, &source.id).unwrap();
        let refused =
            handle_causal_clear_revocation_fence(State(state), admin_headers, Json(request()))
                .await;
        assert_eq!(refused.status(), axum::http::StatusCode::CONFLICT);
    }
}

/// Authenticate + authorize a `/api/device/*` REST caller: valid JWT
/// (header or `token` query — browser download links can't set a header),
/// password already changed, admin role, AND appliance mode. Mirrors
/// `authorize_file_access` but additionally enforces the appliance gate —
/// every `device.*` surface (RPC and REST alike) is appliance-only, per
/// `handlers.rs`'s `require_appliance!()` doc comment.
fn authorize_device_admin(
    state: &AppState,
    headers: &axum::http::HeaderMap,
    token_query: Option<&str>,
) -> Result<(), axum::response::Response> {
    let unauthorized = || {
        (
            axum::http::StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({ "error": "invalid or expired token" })),
        )
            .into_response()
    };
    let token = extract_bearer_token(headers)
        .or(token_query)
        .ok_or_else(unauthorized)?;
    let ctx = authenticate_jwt(state, token).map_err(|_| unauthorized())?;
    require_password_changed(&ctx)?;
    if !ctx.is_admin() {
        return Err((
            axum::http::StatusCode::FORBIDDEN,
            Json(serde_json::json!({ "error": "access denied" })),
        )
            .into_response());
    }
    if !duduclaw_core::is_appliance() {
        return Err((
            axum::http::StatusCode::FORBIDDEN,
            Json(serde_json::json!({
                "error": "此功能僅限 DuDuClaw 裝置版（appliance image）使用。",
                "code": crate::handlers::DEVICE_NOT_APPLIANCE_ERROR_CODE,
            })),
        )
            .into_response());
    }
    Ok(())
}

#[derive(serde::Deserialize)]
struct FilesListQuery {
    agent: Option<String>,
    /// I-4: search — archived name / display name / origin, case-insensitive.
    q: Option<String>,
    /// I-4: filter to files the I-2b ledger ties to this task id.
    task_id: Option<String>,
    /// I-4: inclusive lower bound on mtime, Unix epoch ms.
    since: Option<u64>,
    /// I-4: inclusive upper bound on mtime, Unix epoch ms.
    until: Option<u64>,
}

/// GET /api/files — list attachment files for an agent (or the shared dir).
async fn handle_files_list(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    axum::extract::Query(q): axum::extract::Query<FilesListQuery>,
) -> axum::response::Response {
    let agent = q.agent.as_deref().filter(|s| !s.is_empty());
    if let Err(resp) = authorize_file_access(&state, &headers, None, agent) {
        return resp;
    }
    let dir = match crate::files_api::attachments_dir(&state.home_dir, agent) {
        Some(d) => d,
        None => {
            return (
                axum::http::StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": "invalid agent id" })),
            )
                .into_response();
        }
    };
    let mut files = crate::files_api::list_files(&dir);
    // I-2b: join the provenance ledger so the panel can say which task / AI
    // staff member delivered a file versus which files a human sent in.
    let index = crate::artifacts::provenance_index(&state.home_dir, agent);
    crate::files_api::attach_provenance(&mut files, &index);
    // I-4: search / task-relation / date-range filters — applied after
    // provenance so `q` can match the ledger's display name and origin, not
    // just the raw on-disk archived name. All optional; the default filter
    // is a no-op so this is byte-identical to pre-I-4 behavior when unused.
    let filter = crate::files_api::FileListFilter {
        query: q.q,
        task_id: q.task_id,
        since_ms: q.since,
        until_ms: q.until,
    };
    let files = crate::files_api::filter_files(files, &filter);
    Json(serde_json::json!({ "files": files })).into_response()
}

#[derive(serde::Deserialize)]
struct FilesDownloadQuery {
    agent: Option<String>,
    name: String,
    /// Optional JWT for browser preview/download links that cannot set an
    /// `Authorization` header (`window.open` / `<a href>`).
    token: Option<String>,
}

/// GET /api/files/download — stream a single attachment file.
async fn handle_files_download(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    axum::extract::Query(q): axum::extract::Query<FilesDownloadQuery>,
) -> axum::response::Response {
    let agent = q.agent.as_deref().filter(|s| !s.is_empty());
    // Auth (header or `token` query) + per-agent authorization, fail-closed.
    if let Err(resp) = authorize_file_access(&state, &headers, q.token.as_deref(), agent) {
        return resp;
    }

    let dir = match crate::files_api::attachments_dir(&state.home_dir, agent) {
        Some(d) => d,
        None => {
            return (
                axum::http::StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": "invalid agent id" })),
            )
                .into_response();
        }
    };

    let path = match crate::files_api::resolve_download(&dir, &q.name) {
        Ok(p) => p,
        Err(crate::files_api::ResolveError::BadRequest) => {
            return (
                axum::http::StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": "invalid file name" })),
            )
                .into_response();
        }
        Err(crate::files_api::ResolveError::Denied) => {
            return (
                axum::http::StatusCode::FORBIDDEN,
                Json(serde_json::json!({ "error": "access denied" })),
            )
                .into_response();
        }
        Err(crate::files_api::ResolveError::NotFound) => {
            return (
                axum::http::StatusCode::NOT_FOUND,
                Json(serde_json::json!({ "error": "file not found" })),
            )
                .into_response();
        }
    };

    let file = match tokio::fs::File::open(&path).await {
        Ok(f) => f,
        Err(_) => {
            return (
                axum::http::StatusCode::NOT_FOUND,
                Json(serde_json::json!({ "error": "file not found" })),
            )
                .into_response();
        }
    };

    let stream = tokio_util::io::ReaderStream::new(file);
    let body = axum::body::Body::from_stream(stream);
    let ct = crate::files_api::content_type_for(&q.name);
    let disposition = if crate::files_api::is_inline_previewable(&q.name) {
        "inline"
    } else {
        "attachment"
    };
    // RFC 5987 filename* keeps CJK filenames intact across the header.
    let cd = format!(
        "{disposition}; filename*=UTF-8''{}",
        crate::files_api::encode_filename_star(&q.name)
    );

    let mut resp = axum::response::Response::new(body);
    resp.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static(ct),
    );
    resp.headers_mut().insert(
        axum::http::header::CONTENT_DISPOSITION,
        axum::http::HeaderValue::from_str(&cd)
            .unwrap_or_else(|_| axum::http::HeaderValue::from_static("attachment")),
    );
    resp
}

/// GET /api/files/preview — in-browser preview for office documents.
///
/// Natively-previewable types (pdf/images) stream inline directly. Office
/// types (docx/xlsx/pptx/…) are converted to PDF via LibreOffice headless
/// with an mtime-validated cache under `<home>/cache/preview/<agent>/`;
/// LibreOffice missing → explicit 503 JSON (never a broken byte stream).
/// Same auth + path fences as `handle_files_download`.
async fn handle_files_preview(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    axum::extract::Query(q): axum::extract::Query<FilesDownloadQuery>,
) -> axum::response::Response {
    let agent = q.agent.as_deref().filter(|s| !s.is_empty());
    if let Err(resp) = authorize_file_access(&state, &headers, q.token.as_deref(), agent) {
        return resp;
    }
    let dir = match crate::files_api::attachments_dir(&state.home_dir, agent) {
        Some(d) => d,
        None => {
            return (
                axum::http::StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": "invalid agent id" })),
            )
                .into_response();
        }
    };
    let path = match crate::files_api::resolve_download(&dir, &q.name) {
        Ok(p) => p,
        Err(crate::files_api::ResolveError::BadRequest) => {
            return (
                axum::http::StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": "invalid file name" })),
            )
                .into_response();
        }
        Err(crate::files_api::ResolveError::Denied) => {
            return (
                axum::http::StatusCode::FORBIDDEN,
                Json(serde_json::json!({ "error": "access denied" })),
            )
                .into_response();
        }
        Err(crate::files_api::ResolveError::NotFound) => {
            return (
                axum::http::StatusCode::NOT_FOUND,
                Json(serde_json::json!({ "error": "file not found" })),
            )
                .into_response();
        }
    };

    // Natively previewable → stream the file itself inline.
    if crate::files_api::is_inline_previewable(&q.name) {
        return stream_preview_pdf(&path, &q.name, crate::files_api::content_type_for(&q.name))
            .await;
    }
    if !crate::files_api::is_office_convertible(&q.name) {
        return (
            axum::http::StatusCode::UNSUPPORTED_MEDIA_TYPE,
            Json(serde_json::json!({ "error": "此檔案類型不支援預覽，請下載後開啟" })),
        )
            .into_response();
    }

    // WP-4G: office documents are zip containers, and the file being previewed
    // may be an attachment a stranger sent into a channel. LibreOffice is a
    // recursive-descent OOXML parser with no resource ceiling of its own — a
    // zip bomb or a deeply-nested part would take out the host, not just the
    // conversion. Gate BEFORE the process is spawned; fail-closed on violation.
    let limits = crate::document_limits::DocumentLimits::from_home(&state.home_dir);
    if let Err(v) = crate::document_limits::guard_document_path(&path, &limits) {
        tracing::warn!(
            file = %q.name,
            violation = v.kind(),
            "files preview: refused — document exceeds inbound resource limits"
        );
        return (
            axum::http::StatusCode::PAYLOAD_TOO_LARGE,
            Json(serde_json::json!({ "error": v.user_message(&q.name) })),
        )
            .into_response();
    }

    // Cache: <home>/cache/preview/<agent|_shared>/<stem>.pdf, valid while it
    // is newer than the source file.
    let cache_dir = crate::files_api::preview_cache_dir(&state.home_dir, agent);
    let stem = std::path::Path::new(&q.name)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("preview");
    let cached = cache_dir.join(format!("{stem}.pdf"));
    let src_mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
    let cache_fresh = match (
        std::fs::metadata(&cached).and_then(|m| m.modified()),
        src_mtime,
    ) {
        (Ok(c), Some(s)) => c >= s,
        _ => false,
    };

    if !cache_fresh {
        let Some(soffice) = crate::files_api::find_soffice() else {
            return (
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({
                    "error": "尚未安裝 LibreOffice，無法產生 Office 檔預覽；請下載檔案開啟，或安裝 LibreOffice 後重試"
                })),
            )
                .into_response();
        };
        if let Err(e) = std::fs::create_dir_all(&cache_dir) {
            return (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": format!("preview cache dir failed: {e}") })),
            )
                .into_response();
        }
        // Isolated LO profile: parallel conversions against the default
        // profile fight over its lock and abort.
        let profile = cache_dir.join(".lo_profile");
        let profile_arg = format!("-env:UserInstallation=file://{}", profile.display());
        let run = tokio::time::timeout(
            std::time::Duration::from_secs(60),
            tokio::process::Command::new(&soffice)
                .arg("--headless")
                .arg(profile_arg)
                .arg("--convert-to")
                .arg("pdf")
                .arg("--outdir")
                .arg(&cache_dir)
                .arg(&path)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::piped())
                .output(),
        )
        .await;
        let converted = match run {
            Ok(Ok(out)) if out.status.success() && cached.is_file() => true,
            Ok(Ok(out)) => {
                let raw = String::from_utf8_lossy(&out.stderr);
                let stderr = duduclaw_core::truncate_bytes(&raw, 240);
                tracing::warn!(file = %q.name, %stderr, "files preview: soffice conversion failed");
                false
            }
            Ok(Err(e)) => {
                tracing::warn!(file = %q.name, error = %e, "files preview: soffice spawn failed");
                false
            }
            Err(_) => {
                tracing::warn!(file = %q.name, "files preview: soffice conversion timed out (60s)");
                false
            }
        };
        if !converted {
            return (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": "預覽轉檔失敗，請下載檔案開啟" })),
            )
                .into_response();
        }
    }

    stream_preview_pdf(&cached, &format!("{stem}.pdf"), "application/pdf").await
}

/// Stream `path` inline with `ct` + an RFC 5987 filename header.
async fn stream_preview_pdf(
    path: &std::path::Path,
    filename: &str,
    ct: &'static str,
) -> axum::response::Response {
    let file = match tokio::fs::File::open(path).await {
        Ok(f) => f,
        Err(_) => {
            return (
                axum::http::StatusCode::NOT_FOUND,
                Json(serde_json::json!({ "error": "file not found" })),
            )
                .into_response();
        }
    };
    let stream = tokio_util::io::ReaderStream::new(file);
    let body = axum::body::Body::from_stream(stream);
    let cd = format!(
        "inline; filename*=UTF-8''{}",
        crate::files_api::encode_filename_star(filename)
    );
    let mut resp = axum::response::Response::new(body);
    resp.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static(ct),
    );
    resp.headers_mut().insert(
        axum::http::header::CONTENT_DISPOSITION,
        axum::http::HeaderValue::from_str(&cd)
            .unwrap_or_else(|_| axum::http::HeaderValue::from_static("inline")),
    );
    resp
}

// ── Voice endpoints (openhuman-parity B: STT + TTS) ──────────────

/// Max accepted STT audio upload (10 MiB — a short push-to-talk clip).
const STT_MAX_UPLOAD_BYTES: usize = 10 * 1024 * 1024;

/// Authenticate a request from its `Authorization: Bearer <jwt>` header.
/// Returns `Ok(())` for a valid active-user access token, else an
/// `into_response()`-ready 401. Same stance as `handle_me`.
fn require_bearer(
    state: &AppState,
    headers: &axum::http::HeaderMap,
) -> Result<(), axum::response::Response> {
    let token = extract_bearer_token(headers).ok_or_else(|| {
        (
            axum::http::StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({ "error": "missing Authorization header" })),
        )
            .into_response()
    })?;
    let ctx = authenticate_jwt(state, token).map_err(|_| {
        (
            axum::http::StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({ "error": "invalid or expired token" })),
        )
            .into_response()
    })?;
    require_password_changed(&ctx)
}

/// POST /api/experts/upload — stage an expert-pack `.zip` for installation.
///
/// Multipart body with a `file` (or `pack`) part, ≤50 MiB. Admin-only
/// (Bearer JWT + role check — fail-closed). The upload is staged under
/// `<home>/tmp/expert-uploads/<uuid>-<sanitized-name>.zip` (client filename
/// contributes only a sanitized basename — no traversal) and the resulting
/// server-local path is returned for a follow-up `experts.install` RPC, which
/// runs the full install pipeline (zip-slip fenced extraction + security
/// scanning) on it.
async fn handle_expert_upload(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    mut multipart: Multipart,
) -> axum::response::Response {
    // Bearer + admin — a valid non-admin token is rejected (fail-closed).
    let token = match extract_bearer_token(&headers) {
        Some(t) => t,
        None => {
            return (
                axum::http::StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({ "error": "missing Authorization header" })),
            )
                .into_response();
        }
    };
    let ctx = match authenticate_jwt(&state, token) {
        Ok(c) => c,
        Err(_) => {
            return (
                axum::http::StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({ "error": "invalid or expired token" })),
            )
                .into_response();
        }
    };
    if let Err(resp) = require_password_changed(&ctx) {
        return resp;
    }
    if !ctx.is_admin() {
        return (
            axum::http::StatusCode::FORBIDDEN,
            Json(serde_json::json!({ "error": "只有管理員可以上傳專家包" })),
        )
            .into_response();
    }

    let mut data: Option<Vec<u8>> = None;
    let mut client_name = "pack.zip".to_string();
    loop {
        let field = match multipart.next_field().await {
            Ok(Some(f)) => f,
            Ok(None) => break,
            Err(e) => {
                return (
                    axum::http::StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({ "error": format!("malformed multipart: {e}") })),
                )
                    .into_response();
            }
        };
        match field.name().unwrap_or("") {
            "file" | "pack" => {
                if let Some(fname) = field.file_name() {
                    if !fname.is_empty() {
                        client_name = fname.to_string();
                    }
                }
                match field.bytes().await {
                    Ok(bytes) => {
                        if bytes.len() > crate::expert_admin::MAX_EXPERT_UPLOAD_BYTES {
                            return (
                                axum::http::StatusCode::PAYLOAD_TOO_LARGE,
                                Json(serde_json::json!({
                                    "error": "檔案超過 50 MB 上限"
                                })),
                            )
                                .into_response();
                        }
                        data = Some(bytes.to_vec());
                    }
                    Err(e) => {
                        // axum surfaces the DefaultBodyLimit breach here too.
                        return (
                            axum::http::StatusCode::PAYLOAD_TOO_LARGE,
                            Json(serde_json::json!({
                                "error": format!("讀取上傳內容失敗（檔案過大或連線中斷）: {e}")
                            })),
                        )
                            .into_response();
                    }
                }
            }
            _ => {}
        }
    }

    let Some(data) = data else {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "missing 'file' field" })),
        )
            .into_response();
    };
    // Light sanity: a zip starts with the "PK" local-file signature. The real
    // fence (zip-slip, per-entry caps) runs inside the install pipeline.
    if data.len() < 4 || &data[..2] != b"PK" {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "不是有效的 .zip 檔" })),
        )
            .into_response();
    }

    let dest = crate::expert_admin::staged_upload_path(&state.home_dir, &client_name);
    let dir = crate::expert_admin::upload_dir(&state.home_dir);
    if let Err(e) = tokio::fs::create_dir_all(&dir).await {
        return (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": format!("建立暫存目錄失敗: {e}") })),
        )
            .into_response();
    }
    // Opportunistic cleanup: drop staged uploads older than 24 h.
    if let Ok(mut rd) = tokio::fs::read_dir(&dir).await {
        while let Ok(Some(entry)) = rd.next_entry().await {
            if let Ok(meta) = entry.metadata().await
                && let Ok(modified) = meta.modified()
                && modified
                    .elapsed()
                    .map(|d| d.as_secs() > 86_400)
                    .unwrap_or(false)
            {
                let _ = tokio::fs::remove_file(entry.path()).await;
            }
        }
    }
    if let Err(e) = tokio::fs::write(&dest, &data).await {
        return (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": format!("寫入上傳檔失敗: {e}") })),
        )
            .into_response();
    }

    Json(serde_json::json!({ "path": dest.to_string_lossy() })).into_response()
}

/// POST /api/device/backup-upload — stage an uploaded `.tar.gz` device
/// backup for `device.backup_restore` (WP-G1 device migration / "汰機搬家").
///
/// Multipart body with a `file` part, ≤`MAX_BACKUP_UPLOAD_BYTES`. Admin +
/// appliance gated. The upload is staged under
/// `crate::backup_restore::upload_dir` (client filename contributes only a
/// sanitized basename — no traversal) and the resulting server-local path is
/// returned for the follow-up `device.backup_restore` RPC, which runs the
/// real safety gate (magic check, per-entry/cumulative size caps, path
/// traversal / symlink rejection — `crate::backup_restore`) on it. This
/// endpoint itself only does a cheap magic-byte sanity check, same division
/// of labor as `handle_expert_upload` / `experts.install`.
async fn handle_device_backup_upload(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    mut multipart: Multipart,
) -> axum::response::Response {
    if let Err(resp) = authorize_device_admin(&state, &headers, None) {
        return resp;
    }

    let mut data: Option<Vec<u8>> = None;
    let mut client_name = "backup.tar.gz".to_string();
    loop {
        let field = match multipart.next_field().await {
            Ok(Some(f)) => f,
            Ok(None) => break,
            Err(e) => {
                return (
                    axum::http::StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({ "error": format!("malformed multipart: {e}") })),
                )
                    .into_response();
            }
        };
        if field.name().unwrap_or("") == "file" {
            if let Some(fname) = field.file_name()
                && !fname.is_empty()
            {
                client_name = fname.to_string();
            }
            match field.bytes().await {
                Ok(bytes) => {
                    if bytes.len() > crate::backup_restore::MAX_BACKUP_UPLOAD_BYTES {
                        return (
                            axum::http::StatusCode::PAYLOAD_TOO_LARGE,
                            Json(serde_json::json!({ "error": "備份檔超過上傳上限" })),
                        )
                            .into_response();
                    }
                    data = Some(bytes.to_vec());
                }
                Err(e) => {
                    return (
                        axum::http::StatusCode::PAYLOAD_TOO_LARGE,
                        Json(serde_json::json!({
                            "error": format!("讀取上傳內容失敗（檔案過大或連線中斷）: {e}")
                        })),
                    )
                        .into_response();
                }
            }
        }
    }

    let Some(data) = data else {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "missing 'file' field" })),
        )
            .into_response();
    };
    // Light sanity: a gzip stream starts with the 1f 8b magic. The real
    // fence (tar-entry traversal / symlink / size caps) runs inside
    // `device.backup_restore`.
    if data.len() < 2 || data[0] != 0x1f || data[1] != 0x8b {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "不是有效的 .tar.gz 備份檔" })),
        )
            .into_response();
    }

    let dest = crate::backup_restore::staged_upload_path(&state.home_dir, &client_name);
    let dir = crate::backup_restore::upload_dir(&state.home_dir);
    if let Err(e) = tokio::fs::create_dir_all(&dir).await {
        return (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": format!("建立暫存目錄失敗: {e}") })),
        )
            .into_response();
    }
    // Opportunistic cleanup: drop staged uploads older than 24 h (mirrors
    // `handle_expert_upload`).
    if let Ok(mut rd) = tokio::fs::read_dir(&dir).await {
        while let Ok(Some(entry)) = rd.next_entry().await {
            if let Ok(meta) = entry.metadata().await
                && let Ok(modified) = meta.modified()
                && modified
                    .elapsed()
                    .map(|d| d.as_secs() > 86_400)
                    .unwrap_or(false)
            {
                let _ = tokio::fs::remove_file(entry.path()).await;
            }
        }
    }
    if let Err(e) = tokio::fs::write(&dest, &data).await {
        return (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": format!("寫入上傳檔失敗: {e}") })),
        )
            .into_response();
    }

    Json(serde_json::json!({ "path": dest.to_string_lossy() })).into_response()
}

#[derive(serde::Deserialize)]
struct DeviceBackupDownloadQuery {
    name: String,
    /// Optional JWT for browser download links that cannot set an
    /// `Authorization` header.
    token: Option<String>,
}

/// GET /api/device/backups/download — stream one scheduled backup file from
/// `crate::backup_schedule::backups_dir` (never `attachments/` — see that
/// module's doc comment for why the two stay separate). Admin + appliance
/// gated, same path-safety discipline as `handle_files_download`.
async fn handle_device_backup_download(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    axum::extract::Query(q): axum::extract::Query<DeviceBackupDownloadQuery>,
) -> axum::response::Response {
    if let Err(resp) = authorize_device_admin(&state, &headers, q.token.as_deref()) {
        return resp;
    }

    let dir = crate::backup_schedule::backups_dir(&state.home_dir);
    let path = match crate::files_api::resolve_download(&dir, &q.name) {
        Ok(p) => p,
        Err(crate::files_api::ResolveError::BadRequest) => {
            return (
                axum::http::StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": "invalid file name" })),
            )
                .into_response();
        }
        Err(crate::files_api::ResolveError::Denied) => {
            return (
                axum::http::StatusCode::FORBIDDEN,
                Json(serde_json::json!({ "error": "access denied" })),
            )
                .into_response();
        }
        Err(crate::files_api::ResolveError::NotFound) => {
            return (
                axum::http::StatusCode::NOT_FOUND,
                Json(serde_json::json!({ "error": "file not found" })),
            )
                .into_response();
        }
    };

    let file = match tokio::fs::File::open(&path).await {
        Ok(f) => f,
        Err(_) => {
            return (
                axum::http::StatusCode::NOT_FOUND,
                Json(serde_json::json!({ "error": "file not found" })),
            )
                .into_response();
        }
    };

    let stream = tokio_util::io::ReaderStream::new(file);
    let body = axum::body::Body::from_stream(stream);
    let mut resp = axum::response::Response::new(body);
    resp.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/gzip"),
    );
    let cd = format!(
        "attachment; filename*=UTF-8''{}",
        crate::files_api::encode_filename_star(&q.name)
    );
    resp.headers_mut().insert(
        axum::http::header::CONTENT_DISPOSITION,
        axum::http::HeaderValue::from_str(&cd)
            .unwrap_or_else(|_| axum::http::HeaderValue::from_static("attachment")),
    );
    resp
}

/// POST /api/stt — transcribe an uploaded audio clip to text.
///
/// Multipart body with an `audio` (or `file`) part (webm/ogg/wav, ≤10 MiB) plus
/// an optional `language` text part. Returns `{ "text": "..." }`.
///
/// **Fail-closed**: when STT is unconfigured (`config.toml [voice] stt_provider`
/// unset) this returns HTTP 501 with a friendly zh-TW message — never a guessed
/// or fabricated transcript.
async fn handle_stt(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    mut multipart: Multipart,
) -> axum::response::Response {
    if let Err(resp) = require_bearer(&state, &headers) {
        return resp;
    }

    // Resolve the configured provider first — fail closed before touching the body.
    let provider = match crate::stt::build_provider_from_config(&state.home_dir).await {
        Ok(Some(p)) => p,
        Ok(None) => {
            return (
                axum::http::StatusCode::NOT_IMPLEMENTED,
                Json(serde_json::json!({
                    "error": "尚未設定語音轉文字（STT）。請至「設定 → 語音」選擇 STT 供應商並填入必要欄位後再試。"
                })),
            )
                .into_response();
        }
        Err(e) => {
            return (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": format!("STT 設定錯誤：{e}") })),
            )
                .into_response();
        }
    };

    // Pull the audio + optional language out of the multipart form.
    let mut audio: Option<Vec<u8>> = None;
    let mut filename = "audio.webm".to_string();
    let mut language: Option<String> = None;

    loop {
        let field = match multipart.next_field().await {
            Ok(Some(f)) => f,
            Ok(None) => break,
            Err(e) => {
                return (
                    axum::http::StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({ "error": format!("malformed multipart: {e}") })),
                )
                    .into_response();
            }
        };
        match field.name().unwrap_or("") {
            "audio" | "file" => {
                if let Some(fname) = field.file_name() {
                    if !fname.is_empty() {
                        filename = fname.to_string();
                    }
                }
                match field.bytes().await {
                    Ok(data) => {
                        if let Err(msg) =
                            crate::stt::check_audio_size(data.len(), STT_MAX_UPLOAD_BYTES)
                        {
                            return (
                                axum::http::StatusCode::PAYLOAD_TOO_LARGE,
                                Json(serde_json::json!({ "error": msg })),
                            )
                                .into_response();
                        }
                        audio = Some(data.to_vec());
                    }
                    Err(e) => {
                        return (
                            axum::http::StatusCode::BAD_REQUEST,
                            Json(serde_json::json!({ "error": format!("failed to read audio: {e}") })),
                        )
                            .into_response();
                    }
                }
            }
            "language" => {
                language = field.text().await.ok().filter(|s| !s.is_empty());
            }
            _ => {}
        }
    }

    let audio = match audio {
        Some(a) => a,
        None => {
            return (
                axum::http::StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": "missing 'audio' field" })),
            )
                .into_response();
        }
    };

    match provider
        .transcribe(&audio, &filename, language.as_deref())
        .await
    {
        Ok(text) => Json(serde_json::json!({ "text": text })).into_response(),
        Err(e) => (
            axum::http::StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({ "error": format!("轉錄失敗：{e}") })),
        )
            .into_response(),
    }
}

#[derive(serde::Deserialize)]
struct TtsRequestBody {
    text: String,
    #[serde(default)]
    voice: String,
}

/// POST /api/tts — synthesize speech for `text`, returning audio bytes.
///
/// Reuses `tts.rs` (edge-tts / MiniMax / OpenAI / Piper). The provider strategy
/// follows `inference.toml [voice] tts_provider`. When TTS is explicitly
/// disabled (or no provider is available) this returns HTTP 501 so the client
/// can quietly turn its play toggle off.
async fn handle_tts(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Json(req): Json<TtsRequestBody>,
) -> axum::response::Response {
    use crate::tts::{TtsProvider, TtsRouter, TtsStrategy};

    if let Err(resp) = require_bearer(&state, &headers) {
        return resp;
    }

    let text = req.text.trim();
    if text.is_empty() {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "missing 'text'" })),
        )
            .into_response();
    }

    // Read [voice] tts_provider / tts_voice from inference.toml (where the
    // dashboard Voice tab persists them).
    let (tts_provider, cfg_voice) = {
        let path = state.home_dir.join("inference.toml");
        let table: toml::Table = tokio::fs::read_to_string(&path)
            .await
            .ok()
            .and_then(|c| c.parse().ok())
            .unwrap_or_default();
        let voice = table
            .get("voice")
            .and_then(|v| v.as_table())
            .cloned()
            .unwrap_or_default();
        (
            voice
                .get("tts_provider")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase(),
            voice
                .get("tts_voice")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string(),
        )
    };

    // Explicit opt-out → 501 (client closes its play toggle).
    if matches!(tts_provider.as_str(), "none" | "off" | "disabled") {
        return (
            axum::http::StatusCode::NOT_IMPLEMENTED,
            Json(serde_json::json!({
                "error": "尚未啟用語音朗讀（TTS）。請至「設定 → 語音」選擇語音供應商後再試。"
            })),
        )
            .into_response();
    }

    let strategy = match tts_provider.as_str() {
        "edge-tts" | "edge" => TtsStrategy::EdgeOnly,
        "minimax" | "openai-tts" | "openai" => TtsStrategy::CloudBest,
        _ => TtsStrategy::LocalFirst,
    };

    let models_dir = state.home_dir.join("models");
    let router = TtsRouter::auto_detect(&models_dir, strategy);

    let voice = if req.voice.trim().is_empty() {
        cfg_voice
    } else {
        req.voice.trim().to_string()
    };

    match router.synthesize(text, &voice).await {
        Ok(audio) if !audio.is_empty() => {
            // Sniff the container so the browser <audio> element decodes it.
            let ct = if audio.starts_with(b"RIFF") {
                "audio/wav"
            } else if audio.starts_with(b"OggS") {
                "audio/ogg"
            } else {
                "audio/mpeg"
            };
            let mut resp = axum::response::Response::new(axum::body::Body::from(audio));
            resp.headers_mut().insert(
                axum::http::header::CONTENT_TYPE,
                axum::http::HeaderValue::from_static(ct),
            );
            resp
        }
        Ok(_) => (
            axum::http::StatusCode::NOT_IMPLEMENTED,
            Json(serde_json::json!({
                "error": "尚未啟用語音朗讀（TTS）。請至「設定 → 語音」選擇語音供應商後再試。"
            })),
        )
            .into_response(),
        Err(e) => (
            axum::http::StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({ "error": format!("語音合成失敗：{e}") })),
        )
            .into_response(),
    }
}

/// Authenticate + require an Admin role. Returns `Ok(())` or an
/// `into_response()`-ready 401/403.
fn require_admin_bearer(
    state: &AppState,
    headers: &axum::http::HeaderMap,
) -> Result<(), axum::response::Response> {
    let token = extract_bearer_token(headers).ok_or_else(|| {
        (
            axum::http::StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({ "error": "missing Authorization header" })),
        )
            .into_response()
    })?;
    let ctx = authenticate_jwt(state, token).map_err(|_| {
        (
            axum::http::StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({ "error": "invalid or expired token" })),
        )
            .into_response()
    })?;
    require_password_changed(&ctx)?;
    if !ctx.is_admin() {
        return Err((
            axum::http::StatusCode::FORBIDDEN,
            Json(serde_json::json!({ "error": "admin role required" })),
        )
            .into_response());
    }
    Ok(())
}

/// GET /api/voice/config — read the `[voice]` STT settings from `config.toml`.
///
/// The API key is never returned; instead `stt_api_key_set` reports whether one
/// is stored. This is the source of truth for the STT provider chain that the
/// dashboard Voice tab edits (the general TTS/ASR voice preferences continue to
/// live in `inference.toml [voice]` via `system.update_config`).
async fn handle_voice_config_get(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    if let Err(resp) = require_bearer(&state, &headers) {
        return resp;
    }
    let table: toml::Table = tokio::fs::read_to_string(state.home_dir.join("config.toml"))
        .await
        .ok()
        .and_then(|c| c.parse().ok())
        .unwrap_or_default();
    let voice = table
        .get("voice")
        .and_then(|v| v.as_table())
        .cloned()
        .unwrap_or_default();
    let s = |k: &str| {
        voice
            .get(k)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };
    let key_set = voice
        .get("stt_api_key_enc")
        .and_then(|v| v.as_str())
        .map(|v| !v.is_empty())
        .unwrap_or(false)
        || voice
            .get("stt_api_key")
            .and_then(|v| v.as_str())
            .map(|v| !v.is_empty())
            .unwrap_or(false);
    Json(serde_json::json!({
        "stt_provider": s("stt_provider"),
        "stt_base_url": s("stt_base_url"),
        "stt_model": s("stt_model"),
        "stt_command": s("stt_command"),
        "stt_api_key_set": key_set,
    }))
    .into_response()
}

#[derive(serde::Deserialize)]
struct VoiceConfigBody {
    #[serde(default)]
    stt_provider: String,
    #[serde(default)]
    stt_base_url: String,
    #[serde(default)]
    stt_model: String,
    #[serde(default)]
    stt_command: String,
    /// Omitted / empty → leave the stored key untouched. A literal empty-clear
    /// is done by sending the sentinel `"__CLEAR__"`.
    stt_api_key: Option<String>,
}

/// POST /api/voice/config — write the `[voice]` STT settings to `config.toml`
/// (admin only). The API key is encrypted at rest (AES-256-GCM →
/// `stt_api_key_enc`), matching every other gateway secret.
async fn handle_voice_config_set(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Json(body): Json<VoiceConfigBody>,
) -> axum::response::Response {
    if let Err(resp) = require_admin_bearer(&state, &headers) {
        return resp;
    }

    // Validate provider (fail-closed on typos).
    let provider = body.stt_provider.trim();
    if !provider.is_empty() && crate::stt::parse_provider_kind(provider).is_none() {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": format!("未知的 stt_provider '{provider}'（可用：openai_compat / command）")
            })),
        )
            .into_response();
    }

    let config_path = state.home_dir.join("config.toml");
    let mut table: toml::Table = tokio::fs::read_to_string(&config_path)
        .await
        .ok()
        .and_then(|c| c.parse().ok())
        .unwrap_or_default();

    let voice = table
        .entry("voice".to_string())
        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()));
    let voice = match voice.as_table_mut() {
        Some(v) => v,
        None => {
            return (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": "config.toml [voice] is not a table" })),
            )
                .into_response();
        }
    };

    voice.insert(
        "stt_provider".into(),
        toml::Value::String(provider.to_string()),
    );
    voice.insert(
        "stt_base_url".into(),
        toml::Value::String(body.stt_base_url.trim().to_string()),
    );
    voice.insert(
        "stt_model".into(),
        toml::Value::String(body.stt_model.trim().to_string()),
    );
    voice.insert(
        "stt_command".into(),
        toml::Value::String(body.stt_command.trim().to_string()),
    );

    // API key: encrypt at rest. Empty/absent → keep existing; "__CLEAR__" → wipe.
    match body.stt_api_key.as_deref() {
        None | Some("") => { /* leave stored key untouched */ }
        Some("__CLEAR__") => {
            voice.remove("stt_api_key");
            voice.remove("stt_api_key_enc");
        }
        Some(k) => {
            voice.remove("stt_api_key");
            match crate::config_crypto::encrypt_value(k, &state.home_dir) {
                Some(enc) => {
                    voice.insert("stt_api_key_enc".into(), toml::Value::String(enc));
                }
                None => {
                    return (
                        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                        Json(serde_json::json!({ "error": "failed to encrypt stt_api_key" })),
                    )
                        .into_response();
                }
            }
        }
    }

    // Atomic write: temp file + rename, same pattern as the config handlers.
    let serialized = match toml::to_string_pretty(&table) {
        Ok(s) => s,
        Err(e) => {
            return (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": format!("serialize config.toml: {e}") })),
            )
                .into_response();
        }
    };
    let tmp = config_path.with_extension("toml.tmp");
    if let Err(e) = tokio::fs::write(&tmp, serialized).await {
        return (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": format!("write config.toml: {e}") })),
        )
            .into_response();
    }
    if let Err(e) = tokio::fs::rename(&tmp, &config_path).await {
        let _ = tokio::fs::remove_file(&tmp).await;
        return (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": format!("commit config.toml: {e}") })),
        )
            .into_response();
    }

    Json(serde_json::json!({ "success": true })).into_response()
}

// ── WebSocket Handlers ───────────────────────────────────────

/// Axum handler that upgrades HTTP to WebSocket.
async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
) -> impl IntoResponse {
    // Rate limit: max 30 WS connections per minute per IP.
    if !check_ws_rate_limit(addr.ip()) {
        warn!(ip = %addr.ip(), "WebSocket connection rejected: rate limit exceeded");
        return axum::http::StatusCode::TOO_MANY_REQUESTS.into_response();
    }

    // Validate Origin header to prevent cross-site WebSocket hijacking.
    // Non-browser clients (curl, SDK) don't send Origin, so absent is OK.
    // HS3 fix: exact host match — `starts_with` accepted `localhost.evil.com`.
    if !origin_is_allowed(&headers) {
        let origin = headers
            .get("origin")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        warn!(origin, "WebSocket connection rejected: invalid origin");
        return axum::http::StatusCode::FORBIDDEN.into_response();
    }
    ws.max_message_size(1024 * 1024) // 1MB max WebSocket message
        .on_upgrade(move |socket| handle_socket(socket, state, addr))
}

/// May a credential-less `connect` frame be admitted as a **restricted
/// pre-auth session** — the appliance lock screen asking for the one thing it
/// is allowed to do before anyone logs in (`power_local`'s module header
/// explains why a login-free power control belongs on a lock screen)?
///
/// Pure, so the whole matrix is unit-testable without an `AppState`, a socket
/// or the process-global `DUDUCLAW_APPLIANCE` env var — same rationale as
/// [`jwt_account_gate`] and `local_session::evaluate`.
///
/// Every condition is a fence, and every fence fails closed:
/// * `has_credential` — a frame that DID present a jwt/token must
///   authenticate or be refused; it never silently degrades to a restricted
///   session (that would turn an expired token into a quiet downgrade).
/// * `explicitly_requested || !ed25519_configured` — an Ed25519 client's own
///   `connect` frame is credential-less by design (the signature arrives in
///   the *next* frame), so on an Ed25519-configured gateway the caller has to
///   say `pre_auth: true` to opt out of the challenge flow. With no Ed25519
///   configured there is no such ambiguity and the marker is optional.
/// * `is_appliance` / `peer_is_loopback` — the same two fences the RPC itself
///   re-checks (`power_local::evaluate`). Checking them here as well means an
///   off-appliance or off-box caller never even gets a session object, and
///   the RPC-level check is defence in depth, not the only guard.
fn pre_auth_handshake_allowed(
    has_credential: bool,
    explicitly_requested: bool,
    ed25519_configured: bool,
    is_appliance: bool,
    peer_is_loopback: bool,
) -> bool {
    !has_credential
        && (explicitly_requested || !ed25519_configured)
        && is_appliance
        && peer_is_loopback
}

/// The `UserContext` a restricted pre-auth (lock-screen) connection carries.
///
/// Deliberately NOT `UserContext::admin_fallback()`: the dispatch-top
/// allowlist (`handlers.rs`) is what actually restricts such a connection, and
/// if that allowlist ever had a hole the blast radius must be "the lowest role
/// in the system, bound to no agent", not "full admin". `user_id`/`email` name
/// the surface honestly rather than impersonating a real account, so audit
/// rows never claim a person did this.
fn pre_auth_context() -> UserContext {
    UserContext {
        user_id: "lockscreen".to_string(),
        email: "lockscreen@local".to_string(),
        role: duduclaw_auth::UserRole::Employee,
        agent_access: HashMap::new(),
        must_change_password: false,
    }
}

/// Process a single WebSocket connection.
///
/// `peer` is the connection's real TCP address, forwarded from
/// [`ws_handler`]'s `ConnectInfo` — the ONLY source of "did this come from the
/// machine itself" used anywhere downstream. A request header is never
/// consulted for that question: headers are caller-controlled, which is
/// exactly how a "localhost only" check gets bypassed.
async fn handle_socket(mut socket: WebSocket, state: Arc<AppState>, peer: SocketAddr) {
    info!("New WebSocket connection established");

    // Set only by the credential-less lock-screen branch below; every other
    // authentication path leaves it false, so `RpcConnInfo::pre_auth` (and
    // with it the dispatch-top allowlist) is opt-in, never a fallback.
    let mut pre_auth = false;

    // --- Authentication gate ---
    // Resolve a UserContext from the first "connect" message.
    // Supports 3 modes:
    //   1. JWT token: { "method": "connect", "params": { "jwt": "..." } }
    //   2. Legacy token: { "method": "connect", "params": { "token": "..." } }
    //   3. Ed25519 challenge-response (existing flow)
    //   4. No auth configured: admin fallback

    let user_ctx: UserContext = if state.auth.is_auth_required() || has_users(&state.user_db) {
        // Timeout auth handshake to prevent Slowloris-style resource exhaustion (BE-C4)
        let auth_timeout = std::time::Duration::from_secs(10);
        let result = match tokio::time::timeout(auth_timeout, socket.recv()).await {
            Err(_) => {
                warn!("WebSocket auth timeout — closing connection");
                let _ = socket.send(Message::Close(None)).await;
                return;
            }
            Ok(recv_result) => match recv_result {
                Some(Ok(Message::Text(text))) => {
                    // A frame that does not deserialize is a *protocol* problem,
                    // not a credential one. Both used to end at the same
                    // "auth failed" log, which sent a client-protocol bug
                    // (JSON-RPC 2.0 frames instead of `WsFrame`) on a long
                    // detour through credential debugging. Name it here.
                    if serde_json::from_str::<WsFrame>(&text).is_err() {
                        warn!(
                            "WebSocket handshake frame is not a valid WsFrame \
                             (expected {{\"type\":\"req\",\"method\":\"connect\",…}}) \
                             — this is a client protocol error, not bad credentials"
                        );
                    }
                    match serde_json::from_str::<WsFrame>(&text) {
                        Ok(WsFrame::Request { id, method, params }) if method == "connect" => {
                            // Credentials are read once, trimmed, and blanks
                            // treated as absent — `{"jwt": ""}` is a caller
                            // that presented nothing, not a caller presenting
                            // an empty token, and the two must not take
                            // different branches.
                            let jwt_param = params
                                .get("jwt")
                                .and_then(|v| v.as_str())
                                .map(str::trim)
                                .filter(|s| !s.is_empty());
                            let token_param = params
                                .get("token")
                                .and_then(|v| v.as_str())
                                .map(str::trim)
                                .filter(|s| !s.is_empty());
                            let pre_auth_ok = pre_auth_handshake_allowed(
                                jwt_param.is_some() || token_param.is_some(),
                                params.get("pre_auth").and_then(|v| v.as_bool()) == Some(true),
                                state.auth.is_ed25519(),
                                duduclaw_core::is_appliance(),
                                crate::power_local::ip_is_loopback(peer.ip()),
                            );

                            // ── JWT authentication (new) ─────────────────────
                            if let Some(jwt_str) = jwt_param {
                                match authenticate_jwt(&state, jwt_str) {
                                    Ok(ctx) => {
                                        // `must_change_password` is surfaced here so a
                                        // future frontend can route straight to a
                                        // change-password screen on the handshake
                                        // response itself, instead of waiting to
                                        // discover the restriction from the first
                                        // rejected RPC (handlers.rs::is_password_change_allowlisted).
                                        let ok = WsFrame::ok_response(
                                            &id,
                                            serde_json::json!({
                                                "status": "authenticated",
                                                "user": {
                                                    "id": ctx.user_id,
                                                    "email": ctx.email,
                                                    "role": ctx.role.to_string(),
                                                },
                                                "must_change_password": ctx.must_change_password,
                                            }),
                                        );
                                        let _ = socket
                                            .send(Message::Text(
                                                serde_json::to_string(&ok)
                                                    .unwrap_or_default()
                                                    .into(),
                                            ))
                                            .await;
                                        Ok(ctx)
                                    }
                                    Err(e) => {
                                        let err = WsFrame::error_response(
                                            &id,
                                            &format!("JWT authentication failed: {e}"),
                                        );
                                        let _ = socket
                                            .send(Message::Text(
                                                serde_json::to_string(&err)
                                                    .unwrap_or_default()
                                                    .into(),
                                            ))
                                            .await;
                                        Err(())
                                    }
                                }
                            }
                            // ── Restricted pre-auth (appliance lock screen) ─────
                            // Ordered AFTER the JWT branch (a presented
                            // credential always authenticates or fails) and
                            // BEFORE Ed25519/legacy-token, guarded by
                            // `pre_auth_handshake_allowed` so it can only ever
                            // win for a credential-less caller sitting at an
                            // appliance. The session it grants is restricted at
                            // the RPC dispatch chokepoint
                            // (`handlers.rs`'s pre-auth allowlist) to exactly
                            // `power_local::PRE_AUTH_ALLOWED_METHOD` — the same
                            // "handshake succeeds, dispatch-top allowlist
                            // restricts" shape the bootstrap-admin deadlock fix
                            // established for `users.change_password`.
                            else if pre_auth_ok {
                                pre_auth = true;
                                let ok = WsFrame::ok_response(
                                    &id,
                                    serde_json::json!({ "status": "pre_auth" }),
                                );
                                let _ = socket
                                    .send(Message::Text(
                                        serde_json::to_string(&ok).unwrap_or_default().into(),
                                    ))
                                    .await;
                                info!(peer = %peer.ip(), "lock-screen pre-auth WebSocket session granted");
                                Ok(pre_auth_context())
                            }
                            // ── Ed25519 challenge-response ──────────────────────
                            else if state.auth.is_ed25519() {
                                // M23: challenge is per-connection — held in this
                                // local and threaded into verify_ed25519 below, so
                                // concurrent handshakes never clobber each other.
                                let (challenge_b64, challenge) = state.auth.issue_challenge();
                                let resp = WsFrame::ok_response(
                                    &id,
                                    serde_json::json!({ "challenge": challenge_b64 }),
                                );
                                let _ = socket
                                    .send(Message::Text(
                                        serde_json::to_string(&resp).unwrap_or_default().into(),
                                    ))
                                    .await;

                                // Wait for the `authenticate` message (with timeout)
                                match tokio::time::timeout(auth_timeout, socket.recv())
                                    .await
                                    .unwrap_or(None)
                                {
                                    Some(Ok(Message::Text(auth_text))) => {
                                        match serde_json::from_str::<WsFrame>(&auth_text) {
                                            Ok(WsFrame::Request {
                                                id: auth_id,
                                                method: auth_method,
                                                params: auth_params,
                                            }) if auth_method == "authenticate" => {
                                                let sig = auth_params
                                                    .get("signature")
                                                    .and_then(|v| v.as_str())
                                                    .unwrap_or("");
                                                match state.auth.verify_ed25519(sig, &challenge) {
                                                    Ok(()) => {
                                                        let ok = WsFrame::ok_response(
                                                            &auth_id,
                                                            serde_json::json!({"status": "authenticated"}),
                                                        );
                                                        let _ = socket
                                                            .send(Message::Text(
                                                                serde_json::to_string(&ok)
                                                                    .unwrap_or_default()
                                                                    .into(),
                                                            ))
                                                            .await;
                                                        // Ed25519 users get admin context (backward compat)
                                                        Ok(UserContext::admin_fallback())
                                                    }
                                                    Err(_) => {
                                                        let err = WsFrame::error_response(
                                                            &auth_id,
                                                            "Ed25519 authentication failed",
                                                        );
                                                        let _ = socket
                                                            .send(Message::Text(
                                                                serde_json::to_string(&err)
                                                                    .unwrap_or_default()
                                                                    .into(),
                                                            ))
                                                            .await;
                                                        Err(())
                                                    }
                                                }
                                            }
                                            _ => {
                                                let err = WsFrame::error_response(
                                                    "",
                                                    "expected authenticate message",
                                                );
                                                let _ = socket
                                                    .send(Message::Text(
                                                        serde_json::to_string(&err)
                                                            .unwrap_or_default()
                                                            .into(),
                                                    ))
                                                    .await;
                                                Err(())
                                            }
                                        }
                                    }
                                    _ => Err(()),
                                }
                            }
                            // ── Legacy token authentication ────────────────────
                            else if state.auth.is_auth_required() {
                                let token =
                                    params.get("token").and_then(|v| v.as_str()).unwrap_or("");
                                match state.auth.validate(token) {
                                    Ok(()) => {
                                        let ok = WsFrame::ok_response(
                                            &id,
                                            serde_json::json!({"status": "authenticated"}),
                                        );
                                        let _ = socket
                                            .send(Message::Text(
                                                serde_json::to_string(&ok)
                                                    .unwrap_or_default()
                                                    .into(),
                                            ))
                                            .await;
                                        // Legacy token users get admin context (backward compat)
                                        Ok(UserContext::admin_fallback())
                                    }
                                    Err(_) => {
                                        let err =
                                            WsFrame::error_response(&id, "authentication failed");
                                        let _ = socket
                                            .send(Message::Text(
                                                serde_json::to_string(&err)
                                                    .unwrap_or_default()
                                                    .into(),
                                            ))
                                            .await;
                                        Err(())
                                    }
                                }
                            }
                            // ── User DB exists but no legacy auth — require JWT ──
                            else {
                                let err = WsFrame::error_response(
                                    &id,
                                    "authentication required — provide jwt parameter",
                                );
                                let _ = socket
                                    .send(Message::Text(
                                        serde_json::to_string(&err).unwrap_or_default().into(),
                                    ))
                                    .await;
                                Err(())
                            }
                        }
                        _ => {
                            let err = WsFrame::error_response("", "expected connect message");
                            let _ = socket
                                .send(Message::Text(
                                    serde_json::to_string(&err).unwrap_or_default().into(),
                                ))
                                .await;
                            Err(())
                        }
                    }
                }
                _ => Err(()),
            }, // match recv_result
        }; // match tokio::time::timeout

        match result {
            Ok(ctx) => ctx,
            Err(()) => {
                warn!("WebSocket auth failed – closing connection");
                let _ = socket.send(Message::Close(None)).await;
                return;
            }
        }
    } else {
        // No auth required and no users in DB — admin fallback (local-only dashboard)
        UserContext::admin_fallback()
    };

    info!(user = %user_ctx.email, role = %user_ctx.role, pre_auth, "WebSocket authenticated");

    // Transport facts for this connection, resolved once and carried on every
    // RPC it makes: the real TCP peer (the sole basis for "is this caller
    // sitting at the machine") and whether the handshake was credential-less.
    let conn_info = crate::power_local::RpcConnInfo::from_ws(peer, pre_auth);

    // Split the socket so we can drive sending and receiving concurrently.
    let (mut sink, mut stream) = socket.split();
    let mut log_rx = state.tx.subscribe();
    let mut event_rx = state.event_tx.subscribe();
    let mut logs_subscribed = false;

    // ── P4-3+: OS-native live event tail (opt-in, admin-gated) ────────────
    // A fresh `Receiver` scoped to THIS connection — dropping it (loop exit /
    // connection close, below) unsubscribes from the broadcast automatically,
    // so there is no separate cleanup path to forget. `None` only in the
    // narrow startup window before the gateway has called
    // `set_autopilot_event_tx`; the `os_ev` select arm below never resolves
    // in that case (see its `std::future::pending()` fallback), so it is safe
    // to leave permanently `None` for this connection's lifetime rather than
    // re-checking on every loop iteration.
    let mut os_rx = state
        .handler
        .autopilot_event_tx()
        .await
        .map(|tx| tx.subscribe());
    let mut os_events_subscribed = false;
    // Per-connection sliding-1s forwarding cap (os_events::rate_limit_tick) —
    // `conn_start` is an arbitrary zero point; only elapsed-ms deltas matter.
    let conn_start = std::time::Instant::now();
    let mut os_window_start_ms: u64 = 0;
    let mut os_window_count: u32 = 0;
    let mut os_dropped: u32 = 0;

    // Heartbeat: send ping every 30s, close if no pong in 60s
    let mut heartbeat_interval = tokio::time::interval(std::time::Duration::from_secs(30));
    let mut last_pong = std::time::Instant::now();

    // RPC responses funnel: requests are handled in spawned tasks (see the
    // Request arm below) and their responses come back through this channel.
    // Handling them inline used to stall the whole select loop — a long RPC
    // (experts.generate/install run minutes) stopped the heartbeat arm, the
    // 60s pong check then killed the connection MID-REQUEST (dashboard saw
    // "Connection closed") and every other RPC on the socket was head-of-line
    // blocked. The Option<bool> is the response-gated os_events_subscribed
    // update (see the os.events.subscribe authorization note below).
    let (rpc_tx, mut rpc_rx) = tokio::sync::mpsc::channel::<(WsFrame, Option<bool>)>(64);

    loop {
        tokio::select! {
            // ── Heartbeat ping ─────────────────────────────
            _ = heartbeat_interval.tick() => {
                if last_pong.elapsed().as_secs() > 60 {
                    warn!("Dashboard WebSocket heartbeat timeout");
                    break;
                }
                if sink.send(Message::Ping(vec![].into())).await.is_err() {
                    break;
                }
            }
            // ── Incoming WebSocket frames ───────────────────
            msg_opt = stream.next() => {
                let msg = match msg_opt {
                    Some(Ok(m)) => m,
                    Some(Err(e)) => { warn!("WebSocket receive error: {e}"); break; }
                    None => break,
                };

                #[allow(clippy::collapsible_match)]
                match msg {
                    Message::Text(text) => {
                        let frame = match serde_json::from_str::<WsFrame>(&text) {
                            Ok(f) => f,
                            Err(e) => {
                                error!("Failed to parse WsFrame: {e}");
                                let err_resp = WsFrame::error_response("", "invalid frame");
                                let resp_text = serde_json::to_string(&err_resp).unwrap_or_default();
                                if sink.send(Message::Text(resp_text.into())).await.is_err() { break; }
                                continue;
                            }
                        };

                        match frame {
                            WsFrame::Request { id, method, params } => {
                                // Track log subscription state (method-name
                                // based, so it stays synchronous here).
                                if method == "logs.subscribe" {
                                    logs_subscribed = true;
                                } else if method == "logs.unsubscribe" {
                                    logs_subscribed = false;
                                }

                                // Handle the request in a spawned task — never
                                // inline. See the `rpc_tx` comment above: a
                                // minutes-long RPC awaited here starves the
                                // heartbeat and head-of-line-blocks the socket.
                                let task_state = state.clone();
                                let task_ctx = user_ctx.clone();
                                let task_tx = rpc_tx.clone();
                                tokio::spawn(async move {
                                    let mut response = task_state
                                        .handler
                                        .handle_conn(&method, params, &task_ctx, conn_info)
                                        .await;

                                    // P4-3+ OS live event tail: unlike `logs.subscribe` above
                                    // (which flips its flag on the method NAME alone, before
                                    // authorization runs), gate this flag on the ACTUAL response
                                    // outcome. os_file/os_frontmost events can carry filesystem
                                    // paths and window titles, so a denied (non-admin)
                                    // `os.events.subscribe` must never start the forwarding
                                    // tail. The flag itself lives in the select loop, so the
                                    // decision travels back beside the response.
                                    let os_update = match method.as_str() {
                                        "os.events.subscribe" => {
                                            matches!(&response, WsFrame::Response { ok: true, .. })
                                                .then_some(true)
                                        }
                                        "os.events.unsubscribe" => Some(false),
                                        _ => None,
                                    };

                                    if let WsFrame::Response { id: ref mut resp_id, .. } = response {
                                        *resp_id = id;
                                    }
                                    let _ = task_tx.send((response, os_update)).await;
                                });
                            }
                            other => { warn!("Received non-request frame: {:?}", other); }
                        }
                    }
                    Message::Close(_) => { info!("WebSocket connection closed by client"); break; }
                    Message::Ping(data) => {
                        if sink.send(Message::Pong(data)).await.is_err() { break; }
                    }
                    Message::Pong(_) => {
                        last_pong = std::time::Instant::now();
                    }
                    _ => {}
                }
            }

            // ── Completed RPC responses (handled in spawned tasks) ─
            // `rpc_tx` is held by this scope, so recv() can only yield None
            // after every in-flight task dropped its clone AND the local
            // sender was dropped — which never happens while this loop runs.
            Some((response, os_update)) = rpc_rx.recv() => {
                match os_update {
                    Some(true) => {
                        os_events_subscribed = true;
                        os_window_start_ms = 0;
                        os_window_count = 0;
                    }
                    Some(false) => { os_events_subscribed = false; }
                    None => {}
                }
                let resp_text = serde_json::to_string(&response).unwrap_or_default();
                if sink.send(Message::Text(resp_text.into())).await.is_err() { break; }
            }

            // ── Outbound log broadcast (only when subscribed) ─
            log_line = log_rx.recv(), if logs_subscribed => {
                match log_line {
                    Ok(line) => {
                        // Send as WsFrame::Event so the frontend can parse it uniformly
                        let data = serde_json::from_str::<serde_json::Value>(&line)
                            .unwrap_or(serde_json::Value::String(line));
                        let push = WsFrame::Event {
                            event: "logs.entry".to_string(),
                            payload: data,
                            seq: None,
                            state_version: None,
                        };
                        let text = serde_json::to_string(&push).unwrap_or_default();
                        if sink.send(Message::Text(text.into())).await.is_err() { break; }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {} // drop missed events
                    Err(_) => break,
                }
            }

            // ── Outbound event broadcast (always active for authenticated clients) ─
            event_line = event_rx.recv() => {
                match event_line {
                    Ok(json) => {
                        // Events are already serialized as WsFrame::Event JSON
                        if sink.send(Message::Text(json.into())).await.is_err() { break; }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {} // drop missed events
                    Err(_) => break,
                }
            }

            // ── Outbound OS live-event tail (P4-3+; admin-gated opt-in, rate-capped) ─
            // Wrapped in an async block so the `Option<Receiver>` unwrap only ever
            // runs while the guard is true; when `os_rx` is `None` (autopilot event
            // bus not wired yet) the branch pends forever instead of panicking.
            os_ev = async {
                match os_rx.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            }, if os_events_subscribed => {
                match os_ev {
                    Ok(ev) => {
                        if let Some(payload) = crate::os_events::os_event_push_payload(&ev) {
                            let now_ms = conn_start.elapsed().as_millis() as u64;
                            let (allow, new_start, new_count) = crate::os_events::rate_limit_tick(
                                os_window_start_ms,
                                os_window_count,
                                crate::os_events::OS_EVENTS_PUSH_CAP_PER_SEC,
                                now_ms,
                            );
                            os_window_start_ms = new_start;
                            os_window_count = new_count;
                            if allow {
                                let push = WsFrame::Event {
                                    event: "os.events.entry".to_string(),
                                    payload,
                                    seq: None,
                                    state_version: None,
                                };
                                let text = serde_json::to_string(&push).unwrap_or_default();
                                if sink.send(Message::Text(text.into())).await.is_err() { break; }
                            } else {
                                os_dropped += 1;
                                if os_dropped == 1 || os_dropped % 100 == 0 {
                                    warn!(
                                        dropped = os_dropped,
                                        "os.events live tail: per-connection rate cap ({} /s) exceeded — dropping",
                                        crate::os_events::OS_EVENTS_PUSH_CAP_PER_SEC,
                                    );
                                }
                            }
                        }
                        // Non-OS AutopilotEvent variants (TaskCreated, AgentIdle, ...)
                        // are silently ignored — this tail forwards os_file/os_frontmost only.
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {} // drop missed events
                    Err(broadcast::error::RecvError::Closed) => {
                        // Autopilot event bus torn down — stop polling a dead
                        // receiver instead of hot-looping on repeated `Closed`.
                        os_rx = None;
                        os_events_subscribed = false;
                    }
                }
            }
        }
    }

    info!("WebSocket connection terminated");
}

/// Pure decision extracted from [`authenticate_jwt`] so the fix for
/// `wiki/reports/resolved-todos/TODO-bootstrap-admin-ws-deadlock.md` is unit-testable without a
/// full `AppState`/DB fixture (same rationale as `is_enterprise_only_method`
/// in handlers.rs). Given the account status + forced-password-change flag
/// from a *fresh* DB read (never the JWT's own claims — a token can outlive
/// a password change), decides whether the account may authenticate at all
/// and, if so, whether the session should carry the forced-password-change
/// restriction.
///
/// Before this fix, `must_change_password = true` made this whole function
/// return `Err`, which refused the WS handshake outright — indistinguishable,
/// from the frontend's perspective, from a hung connection (`/api/login` and
/// `/api/me` never consulted this gate, so nothing warned the caller before
/// the socket connect). This check is address-blind: unlike
/// `local_session::evaluate`'s Personal+loopback auto-login escape hatch, it
/// authenticates a LAN client hitting an Enterprise container exactly the
/// same as a loopback one — the restriction that follows is enforced at the
/// RPC dispatch chokepoint (`handlers.rs::is_password_change_allowlisted`),
/// not by refusing to authenticate.
fn jwt_account_gate(
    status: duduclaw_auth::UserStatus,
    must_change_password: bool,
) -> Result<bool, String> {
    if status != duduclaw_auth::UserStatus::Active {
        return Err("account is suspended or offboarded".to_string());
    }
    Ok(must_change_password)
}

/// Verify a JWT access token and build a UserContext.
/// Single DB lookup, fail-closed on error (R2 fix for double-lookup + fail-open).
fn authenticate_jwt(state: &AppState, jwt_str: &str) -> Result<UserContext, String> {
    let claims = state.jwt_config.verify_access_token(jwt_str)?;

    // Single DB lookup — fail-closed: DB error = reject.
    let must_change_password = match state.user_db.get_user(&claims.sub) {
        Ok(Some(user)) => jwt_account_gate(user.status, user.must_change_password)?,
        Ok(None) => return Err("user not found".to_string()),
        Err(_) => return Err("authentication service unavailable".to_string()),
    };

    // The handshake succeeds for every Active account, flagged or not.
    // `must_change_password` rides along on the UserContext; the RPC
    // dispatch chokepoint restricts such a caller to the self-service
    // change-password allowlist until the flag clears (C1's original intent
    // — block all operations — is preserved, just enforced one layer up).
    UserContext::from_claims(&claims, must_change_password)
}

/// Check if any users exist in the database (to decide whether auth is needed).
/// Fail-closed: if the DB query fails, assume users exist and require auth (C2 fix).
fn has_users(user_db: &UserDb) -> bool {
    user_db.list_users().map(|u| !u.is_empty()).unwrap_or(true)
}

/// Simple health-check endpoint.
async fn health_handler() -> &'static str {
    "ok"
}

/// Wall-clock unix seconds when `start_gateway` began (0 = unknown). Gives the
/// `/healthz` scheduler probe a boot reference so "loop never started" can be
/// distinguished from "still booting".
pub(crate) static SERVER_START_UNIX: std::sync::atomic::AtomicI64 =
    std::sync::atomic::AtomicI64::new(0);

/// A scheduler loop is considered dead when its last tick is older than this
/// (loops tick every 30s — 10 missed ticks is far beyond transient load).
const SCHED_STALL_SECS: i64 = 300;

/// Seconds-ago for one scheduler tick timestamp, plus whether it counts as
/// stalled. `last == 0` (never ticked) only counts as stalled once the
/// gateway has been up past the stall window — before that it's "booting".
fn sched_probe(last: i64, now: i64, start: i64) -> (Option<i64>, bool) {
    if last > 0 {
        let ago = (now - last).max(0);
        (Some(ago), ago > SCHED_STALL_SECS)
    } else {
        (None, start > 0 && now - start > SCHED_STALL_SECS)
    }
}

/// JSON liveness probe for the desktop Gateway picker (WP-GW). Returns the
/// gateway version + display name so the picker can show them next to a
/// discovered / manually-entered endpoint. Unauthenticated, like `/health`.
async fn healthz_handler() -> impl IntoResponse {
    let name =
        std::fs::read_to_string(duduclaw_core::platform::duduclaw_home().join("config.toml"))
            .ok()
            .map(|text| crate::mdns::MdnsConfig::from_toml_str(&text, "DuDuClaw").name)
            .unwrap_or_else(|| "DuDuClaw".to_string());

    // Background-scheduler liveness (2026-08 LWM incident: cron/heartbeat
    // silently dead while HTTP kept answering, so Docker showed "healthy"
    // for days of missed schedules). A scheduler loop that has not ticked
    // for SCHED_STALL_SECS — or never started at all after the boot grace
    // window — flips this endpoint to 503 so restart policies can self-heal
    // and monitors actually see the failure.
    use std::sync::atomic::Ordering;
    let now = chrono::Utc::now().timestamp();
    let start = SERVER_START_UNIX.load(Ordering::Relaxed);
    let (cron_ago, cron_stalled) = sched_probe(
        crate::cron_scheduler::LAST_TICK_UNIX.load(Ordering::Relaxed),
        now,
        start,
    );
    let (hb_ago, hb_stalled) = sched_probe(
        duduclaw_agent::heartbeat::LAST_TICK_UNIX.load(Ordering::Relaxed),
        now,
        start,
    );
    let ok = !cron_stalled && !hb_stalled;
    let body = Json(serde_json::json!({
        "ok": ok,
        "service": "duduclaw-gateway",
        "version": env!("CARGO_PKG_VERSION"),
        "name": name,
        "schedulers": {
            "cron_tick_secs_ago": cron_ago,
            "cron_stalled": cron_stalled,
            "heartbeat_tick_secs_ago": hb_ago,
            "heartbeat_stalled": hb_stalled,
        },
    }));
    let status = if ok {
        axum::http::StatusCode::OK
    } else {
        axum::http::StatusCode::SERVICE_UNAVAILABLE
    };
    (status, body)
}

// ── Reliability Dashboard HTTP endpoint (W20-P0) ─────────────

/// Query parameters for `GET /api/reliability/summary`.
#[derive(serde::Deserialize, Debug)]
struct ReliabilitySummaryParams {
    /// Agent ID to compute the summary for (required).
    agent_id: Option<String>,
    /// Measurement window in days (1–365, default 7).
    window_days: Option<u32>,
}

/// GET /api/reliability/summary — Agent Reliability Dashboard Phase 1.
///
/// Returns a JSON object with four reliability metrics for the requested agent
/// over a configurable time window backed by the EvolutionEvent audit trail.
///
/// **Authorization** (M1): requires a valid access-token Bearer header and an
/// allowed `Origin`. Previously unauthenticated, which leaked per-agent
/// reliability metrics and allowed I/O amplification (a full `sync_from_files`
/// ran per request). The index is now shared + background-synced.
///
/// ## Query parameters
/// - `agent_id` (required) — Agent to query.
/// - `window_days` (optional, 1–365, default 7) — Measurement window.
///
/// ## Example
/// ```text
/// curl -H "Authorization: Bearer <token>" \
///   "http://localhost:8080/api/reliability/summary?agent_id=my-agent&window_days=7"
/// ```
async fn handle_reliability_summary_http(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Query(params): Query<ReliabilitySummaryParams>,
) -> impl IntoResponse {
    // M1: enforce Origin + JWT auth at the HTTP layer.
    if !origin_is_allowed(&headers) {
        return (
            axum::http::StatusCode::FORBIDDEN,
            Json(serde_json::json!({"error": "origin not allowed"})),
        )
            .into_response();
    }
    let token = match extract_bearer_token(&headers) {
        Some(t) => t,
        None => {
            return (
                axum::http::StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"error": "missing Authorization header"})),
            )
                .into_response();
        }
    };
    if state.jwt_config.verify_access_token(token).is_err() {
        return (
            axum::http::StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": "invalid or expired token"})),
        )
            .into_response();
    }

    let agent_id = match params.agent_id.as_deref() {
        Some(id) if !id.is_empty() => id.to_owned(),
        _ => {
            return Json(serde_json::json!({
                "error": "agent_id query parameter is required"
            }))
            .into_response();
        }
    };

    let window_days = params.window_days.unwrap_or(7).clamp(1, 365);

    // M1/M60: reuse the shared, background-synced index instead of opening a
    // fresh DB connection and running a full sync on every request.
    let idx = match state.handler.audit_index().await {
        Ok(i) => i,
        Err(e) => {
            warn!("GET /api/reliability/summary: index open failed: {e}");
            return Json(serde_json::json!({
                "error": format!("audit index unavailable: {e}")
            }))
            .into_response();
        }
    };

    match idx
        .compute_reliability_summary(&agent_id, window_days)
        .await
    {
        Ok(s) => Json(serde_json::json!({
            "agent_id":              s.agent_id,
            "window_days":           s.window_days,
            "consistency_score":     s.consistency_score,
            "task_success_rate":     s.task_success_rate,
            "skill_adoption_rate":   s.skill_adoption_rate,
            "fallback_trigger_rate": s.fallback_trigger_rate,
            "total_events":          s.total_events,
            "generated_at":          s.generated_at,
        }))
        .into_response(),
        Err(e) => {
            warn!("GET /api/reliability/summary: compute failed: {e}");
            Json(serde_json::json!({
                "error": format!("reliability computation failed: {e}")
            }))
            .into_response()
        }
    }
}

// ── MCP OAuth callback endpoint ─────────────────────────────

/// Query parameters from the OAuth provider redirect.
#[derive(serde::Deserialize)]
struct OAuthCallbackParams {
    code: String,
    state: String,
}

/// GET /api/mcp/oauth/callback — Handles the OAuth redirect from the provider.
async fn handle_mcp_oauth_callback(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(params): axum::extract::Query<OAuthCallbackParams>,
) -> impl IntoResponse {
    // Look up the pending OAuth flow by state nonce
    let pending = {
        let mut map = state.handler.mcp_oauth_pending().write().await;
        crate::mcp_oauth::cleanup_pending(&mut map);
        map.remove(&params.state)
    };

    let pending = match pending {
        Some(p) => p,
        None => {
            warn!("MCP OAuth callback with unknown state parameter");
            return axum::response::Html(
                "<html><body><h2>Authentication failed</h2>\
                 <p>Unknown or expired OAuth state. Please try again from the dashboard.</p>\
                 </body></html>"
                    .to_string(),
            );
        }
    };

    // Exchange the authorization code for tokens
    let token = match crate::mcp_oauth::exchange_code(
        &pending.config,
        &params.code,
        &pending.code_verifier,
    )
    .await
    {
        Ok(t) => t,
        Err(e) => {
            warn!(provider = %pending.provider_id, error = %e, "MCP OAuth token exchange failed");
            return axum::response::Html(format!(
                "<html><body><h2>Authentication failed</h2>\
                 <p>Token exchange error: {e}</p>\
                 <p>Please close this window and try again.</p>\
                 </body></html>"
            ));
        }
    };

    // Save the token to disk
    let home_dir = state.handler.home_dir();
    if let Err(e) = crate::mcp_oauth::upsert_token(home_dir, token) {
        warn!(error = %e, "Failed to save MCP OAuth token");
        return axum::response::Html(format!(
            "<html><body><h2>Authentication failed</h2>\
             <p>Failed to save token: {e}</p>\
             </body></html>"
        ));
    }

    info!(provider = %pending.provider_id, "MCP OAuth authentication successful");

    // Connecting Google IS the opt-in: flip the `[integrations]
    // google_workspace` gate so the 19 workspace tools actually reach agents.
    // Leaving it to a manual config.toml edit made "connected" a lie — the
    // credential test passed while every tool call dead-ended.
    if pending.provider_id == "google" {
        match crate::google_workspace::enable_integration(home_dir) {
            Ok(true) => info!("enabled [integrations] google_workspace in config.toml"),
            Ok(false) => {}
            Err(e) => {
                warn!(error = %e, "could not auto-enable google_workspace integration; enable it manually in config.toml")
            }
        }
    }
    // H8 (2026-09 feature audit): GitHub now has the same deny-by-default
    // gate Google does, so connecting has to open it for the same reason —
    // otherwise the connection test goes green while every GitHub tool call
    // dead-ends on the closed gate.
    if pending.provider_id == "github" {
        match crate::github_workspace::enable_integration(home_dir) {
            Ok(true) => info!("enabled [integrations] github in config.toml"),
            Ok(false) => {}
            Err(e) => {
                warn!(error = %e, "could not auto-enable github integration; enable it manually in config.toml")
            }
        }
    }

    axum::response::Html(
        "<html><body style=\"font-family: system-ui, sans-serif; display: flex; \
         justify-content: center; align-items: center; height: 100vh; margin: 0; \
         background: #fafaf9;\">\
         <div style=\"text-align: center;\">\
         <h2 style=\"color: #1c1917;\">Authentication Successful</h2>\
         <p style=\"color: #78716c;\">You can close this window and return to the dashboard.</p>\
         </div></body></html>"
            .to_string(),
    )
}

// ── .well-known endpoints for protocol discovery ──────────────

async fn well_known_mcp_server_card() -> axum::Json<serde_json::Value> {
    axum::Json(serde_json::json!({
        "name": "DuDuClaw MCP Server",
        "version": crate::updater::current_version(),
        "description": "Claude Code extension layer with channel routing, memory, agent orchestration, and local inference",
        "tools": [
            {"name": "send_message", "description": "Send message to channel"},
            {"name": "memory_search", "description": "Search agent memory"},
            {"name": "memory_store", "description": "Store memory entry"},
            {"name": "execute_program", "description": "Execute PTC script"},
            {"name": "skill_bank_search", "description": "Search skill bank"},
            {"name": "session_restore_context", "description": "Restore hidden context"},
            {"name": "create_agent", "description": "Create sub-agent"},
            {"name": "send_to_agent", "description": "Delegate to agent"},
        ],
        "capabilities": ["memory", "agents", "channels", "inference", "skills", "evolution"],
    }))
}

#[cfg(test)]
mod login_rate_limit_tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[test]
    fn allows_up_to_five_then_blocks() {
        let ip = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 10));
        let email = "rl-block@test.invalid";
        // 5 attempts permitted, the 6th is blocked.
        for i in 1..=5 {
            assert!(check_login_rate_limit(ip, email), "attempt {i} should pass");
        }
        assert!(
            !check_login_rate_limit(ip, email),
            "6th attempt must be blocked"
        );
    }

    #[test]
    fn reset_on_success_clears_counter() {
        // M2: a successful login clears the counter so the account is not
        // locked out by earlier failures.
        let ip = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 20));
        let email = "rl-reset@test.invalid";
        for _ in 0..5 {
            assert!(check_login_rate_limit(ip, email));
        }
        assert!(
            !check_login_rate_limit(ip, email),
            "should be blocked before reset"
        );
        reset_login_rate_limit(ip, email);
        // After reset the budget is replenished.
        assert!(check_login_rate_limit(ip, email), "should pass after reset");
    }

    #[test]
    fn different_ips_have_independent_budgets() {
        // M2: keying by IP+email prevents one attacker IP from locking out a
        // victim authenticating from a different IP.
        let email = "rl-iso@test.invalid";
        let attacker = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 30));
        let victim = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 31));
        for _ in 0..6 {
            let _ = check_login_rate_limit(attacker, email);
        }
        assert!(
            !check_login_rate_limit(attacker, email),
            "attacker should be blocked"
        );
        // Victim on a different IP is unaffected.
        assert!(
            check_login_rate_limit(victim, email),
            "victim should still pass"
        );
    }
}

#[cfg(test)]
mod origin_allowlist_tests {
    use super::*;

    /// Build a `HeaderMap` carrying a single `Origin` header.
    fn origin_headers(origin: &str) -> axum::http::HeaderMap {
        let mut h = axum::http::HeaderMap::new();
        h.insert("origin", origin.parse().unwrap());
        h
    }

    #[test]
    fn absent_origin_is_allowed() {
        // Non-browser clients (curl/SDK) send no Origin — always allowed.
        let h = axum::http::HeaderMap::new();
        assert!(origin_is_allowed_with(&h, &[]));
    }

    #[test]
    fn loopback_allowed_by_default_external_blocked() {
        // (a) With an empty extra list, only built-in loopback origins pass.
        assert!(origin_is_allowed_with(
            &origin_headers("http://localhost:18789"),
            &[]
        ));
        assert!(origin_is_allowed_with(
            &origin_headers("http://127.0.0.1:5173"),
            &[]
        ));
        assert!(!origin_is_allowed_with(
            &origin_headers("http://evil.example.com"),
            &[]
        ));
    }

    #[test]
    fn configured_origin_allows_exact_match() {
        // (b) After configuring a tailnet host, its Origin is accepted.
        let extra = vec!["box.tailscale.ts.net".to_string()];
        assert!(origin_is_allowed_with(
            &origin_headers("https://box.tailscale.ts.net"),
            &extra
        ));
        // A different host is still blocked.
        assert!(!origin_is_allowed_with(
            &origin_headers("https://other.tailscale.ts.net"),
            &extra
        ));
    }

    #[test]
    fn suffix_attacks_still_blocked() {
        // (c) Suffix/prefix attacks against a configured host must not pass.
        let extra = vec!["localhost".to_string(), "dash.example.com".to_string()];
        assert!(!origin_is_allowed_with(
            &origin_headers("http://localhost.evil.com"),
            &extra
        ));
        assert!(!origin_is_allowed_with(
            &origin_headers("http://evil-localhost.com"),
            &extra
        ));
        assert!(!origin_is_allowed_with(
            &origin_headers("http://dash.example.com.evil.com"),
            &extra
        ));
        assert!(!origin_is_allowed_with(
            &origin_headers("http://evildash.example.com"),
            &extra
        ));
    }

    #[test]
    fn scheme_and_trailing_slash_are_normalized() {
        // (d) Config values with scheme / trailing slash normalize correctly.
        assert_eq!(
            normalize_origin_entry("https://dash.example.com:8080/"),
            Some("dash.example.com:8080".to_string())
        );
        assert_eq!(
            normalize_origin_entry("  ws://box.tailnet.ts.net/  "),
            Some("box.tailnet.ts.net".to_string())
        );
        assert_eq!(
            normalize_origin_entry("HTTP://Host.Example"),
            Some("Host.Example".to_string())
        );
        assert_eq!(normalize_origin_entry("   "), None);
        assert_eq!(normalize_origin_entry("https://"), None);

        // A normalized host:port entry matches only that exact port.
        let extra = vec![normalize_origin_entry("https://dash.example.com:8080/").unwrap()];
        assert!(origin_is_allowed_with(
            &origin_headers("https://dash.example.com:8080"),
            &extra
        ));
        assert!(!origin_is_allowed_with(
            &origin_headers("https://dash.example.com:9090"),
            &extra
        ));
    }

    #[test]
    fn init_filters_empty_entries() {
        // Empty/whitespace/scheme-only entries are dropped during normalization.
        let normalized: Vec<String> = vec![
            "  ".to_string(),
            "https://".to_string(),
            "http://good.host/".to_string(),
        ]
        .iter()
        .filter_map(|s| normalize_origin_entry(s))
        .collect();
        assert_eq!(normalized, vec!["good.host".to_string()]);
    }

    #[test]
    fn hot_update_reflects_immediately_and_preserves_env() {
        // This test drives the process-wide ALLOWED_ORIGINS cell (init + set),
        // so keep it self-contained and restore the env at the end. It is the
        // only test that mutates the global cell / DUDUCLAW_ALLOWED_ORIGINS env.
        let saved_env = std::env::var("DUDUCLAW_ALLOWED_ORIGINS").ok();
        // SAFETY: single-threaded test body; env restored before returning.
        unsafe { std::env::set_var("DUDUCLAW_ALLOWED_ORIGINS", "env.host.ts.net") };

        // Startup: CLI merges config + env, then init installs the combined list.
        init_allowed_origins(vec![
            "https://dash.example.com/".to_string(),
            "env.host.ts.net".to_string(),
        ]);
        assert!(origin_is_allowed(&origin_headers(
            "https://dash.example.com"
        )));
        assert!(origin_is_allowed(&origin_headers(
            "https://env.host.ts.net"
        )));
        assert!(!origin_is_allowed(&origin_headers(
            "https://new.example.com"
        )));

        // Dashboard save: only the config.toml portion is sent (env unknown to UI).
        // A newly-added host is allowed immediately, WITHOUT a restart...
        set_allowed_origins(vec!["https://new.example.com/".to_string()]);
        assert!(origin_is_allowed(&origin_headers(
            "https://new.example.com"
        )));
        // ...the removed config host is now blocked...
        assert!(!origin_is_allowed(&origin_headers(
            "https://dash.example.com"
        )));
        // ...and the env-provided host survives the save (re-merged in the setter).
        assert!(origin_is_allowed(&origin_headers(
            "https://env.host.ts.net"
        )));

        // Clearing the config list back to empty keeps env, drops config hosts.
        set_allowed_origins(vec![]);
        assert!(!origin_is_allowed(&origin_headers(
            "https://new.example.com"
        )));
        assert!(origin_is_allowed(&origin_headers(
            "https://env.host.ts.net"
        )));
        // Loopback always allowed regardless.
        assert!(origin_is_allowed(&origin_headers("http://localhost:8080")));

        // Restore global state so other tests / cargo test ordering is unaffected.
        // SAFETY: single-threaded test body restoring the pre-test env value.
        unsafe {
            match saved_env {
                Some(v) => std::env::set_var("DUDUCLAW_ALLOWED_ORIGINS", v),
                None => std::env::remove_var("DUDUCLAW_ALLOWED_ORIGINS"),
            }
        }
        // Reset the cell to empty for a clean slate.
        *allowed_origins_cell().write().unwrap() = Vec::new();
    }
}

#[cfg(test)]
mod jwt_account_gate_tests {
    use super::*;

    /// TODO-bootstrap-admin-ws-deadlock.md core regression: a flagged but
    /// Active account (the bootstrap `admin@local`, or any operator-reset
    /// account) now authenticates instead of the handshake being refused
    /// outright, AND the flag is reported rather than swallowed. This
    /// function takes no address at all, so it is exactly as true for a LAN
    /// client hitting an Enterprise container over a Docker bridge network
    /// as it is for a loopback caller — the deadlock was address-independent
    /// even though the old symptom (`local_session` 403) looked address-related.
    #[test]
    fn active_but_flagged_account_authenticates_with_the_flag_reported() {
        let must_change_password =
            jwt_account_gate(duduclaw_auth::UserStatus::Active, true).unwrap();
        assert!(
            must_change_password,
            "the handshake must succeed AND report the flag, not refuse the connection"
        );
    }

    #[test]
    fn active_unflagged_account_authenticates_clear() {
        let must_change_password =
            jwt_account_gate(duduclaw_auth::UserStatus::Active, false).unwrap();
        assert!(!must_change_password);
    }

    /// The account-status gate (suspended/offboarded) is untouched by this
    /// fix — only the must-change-password branch stopped refusing the
    /// handshake. A non-Active account is still refused outright, regardless
    /// of the password flag.
    #[test]
    fn non_active_account_is_still_refused_regardless_of_the_flag() {
        assert!(jwt_account_gate(duduclaw_auth::UserStatus::Suspended, false).is_err());
        assert!(jwt_account_gate(duduclaw_auth::UserStatus::Suspended, true).is_err());
        assert!(jwt_account_gate(duduclaw_auth::UserStatus::Offboarded, false).is_err());
    }
}

/// IMPL-POWER — the WS handshake half of the appliance lock screen's
/// login-free power surface. Mirrors `jwt_account_gate_tests`' shape above:
/// the decision is a pure function, so every combination is checked without an
/// `AppState`, a socket, or the process-global `DUDUCLAW_APPLIANCE` env var.
#[cfg(test)]
mod pre_auth_handshake_tests {
    use super::*;

    /// Argument order is easy to transpose, so name them at every call site.
    fn allowed(
        has_credential: bool,
        explicitly_requested: bool,
        ed25519: bool,
        appliance: bool,
        loopback: bool,
    ) -> bool {
        pre_auth_handshake_allowed(
            has_credential,
            explicitly_requested,
            ed25519,
            appliance,
            loopback,
        )
    }

    /// The one accepted shape: no credential, on an appliance, over loopback.
    /// With no Ed25519 configured the explicit marker is optional, so the
    /// shell works whether or not it sends one.
    #[test]
    fn credential_less_loopback_appliance_is_admitted_with_or_without_the_marker() {
        assert!(allowed(false, true, false, true, true));
        assert!(allowed(false, false, false, true, true));
    }

    /// Each fence, failed on its own, refuses — nothing here is advisory.
    #[test]
    fn every_fence_refuses_independently() {
        // Presented a credential: must authenticate or fail, never silently
        // degrade to a restricted session (an expired token is not a lock
        // screen).
        assert!(!allowed(true, true, false, true, true));
        // Not an appliance.
        assert!(!allowed(false, true, false, false, true));
        // Not sitting at the machine.
        assert!(!allowed(false, true, false, true, false));
    }

    /// An Ed25519 client's own `connect` frame is credential-less by design
    /// (the signature arrives in the NEXT frame), so on an Ed25519-configured
    /// gateway the pre-auth branch must not swallow it — only an explicit
    /// `pre_auth: true` opts out of the challenge flow.
    #[test]
    fn ed25519_challenge_flow_is_not_hijacked() {
        assert!(!allowed(false, false, true, true, true));
        assert!(allowed(false, true, true, true, true));
    }

    /// The blanket case worth stating once: off-appliance, NOTHING admits a
    /// pre-auth session — not the marker, not loopback, not both.
    #[test]
    fn off_appliance_nothing_admits_a_pre_auth_session() {
        for &requested in &[true, false] {
            for &ed25519 in &[true, false] {
                for &loopback in &[true, false] {
                    assert!(
                        !allowed(false, requested, ed25519, false, loopback),
                        "requested={requested} ed25519={ed25519} loopback={loopback}"
                    );
                }
            }
        }
    }

    /// The restricted context is the lowest role in the system, bound to no
    /// agent, and does not impersonate a real account — so a hole in the
    /// dispatch-top allowlist could never mean "full admin", and audit rows
    /// never claim a person did this.
    #[test]
    fn pre_auth_context_is_least_privilege_and_honestly_named() {
        let ctx = pre_auth_context();
        assert_eq!(ctx.role, duduclaw_auth::UserRole::Employee);
        assert!(!ctx.is_admin());
        assert!(ctx.agent_access.is_empty());
        assert!(!ctx.must_change_password);
        assert_ne!(ctx.user_id, UserContext::admin_fallback().user_id);
        assert_ne!(ctx.email, "admin@local");
    }

    /// The handshake fence and the RPC fence must read loopback the same way
    /// — including the IPv4-mapped IPv6 form a dual-stack listener reports.
    /// Two implementations that disagree would let one of them be bypassed.
    #[test]
    fn handshake_and_rpc_share_one_loopback_authority() {
        use std::net::IpAddr;
        for (raw, expected) in [
            ("127.0.0.1", true),
            ("::1", true),
            ("::ffff:127.0.0.1", true),
            ("192.168.1.10", false),
            ("::ffff:192.168.1.10", false),
        ] {
            let ip: IpAddr = raw.parse().unwrap();
            assert_eq!(crate::power_local::ip_is_loopback(ip), expected, "{raw}");
        }
    }
}

/// Process-wide A2A signer, initialized once from the on-disk key (generating it
/// on first use). `None` means key load/generation failed — the card is served
/// unsigned (fail-open on availability, fail-closed on integrity: an unsigned
/// card is honest about its lack of a signature). A warning is logged once.
fn a2a_signer() -> Option<&'static crate::a2a_signing::A2aSigner> {
    use std::sync::OnceLock;
    static SIGNER: OnceLock<Option<crate::a2a_signing::A2aSigner>> = OnceLock::new();
    SIGNER
        .get_or_init(|| {
            let path = crate::a2a_signing::default_key_path();
            match crate::a2a_signing::A2aSigner::load_or_generate(&path) {
                Ok((signer, generated)) => {
                    if generated {
                        info!(
                            "已生成 A2A Agent Card 簽章金鑰（{}），公鑰指紋 {}",
                            path.display(),
                            signer.fingerprint()
                        );
                    }
                    Some(signer)
                }
                Err(e) => {
                    warn!("A2A 簽章金鑰不可用，Agent Card 將以未簽章方式提供：{e}");
                    None
                }
            }
        })
        .as_ref()
}

/// Build the unsigned A2A Agent Card body (existing fields, unchanged).
fn build_agent_card() -> serde_json::Value {
    serde_json::json!({
        "name": "DuDuClaw Agent",
        "description": "AI agent with channel routing, memory, and self-evolution",
        // Same shared resolver `duduclaw run` uses (env > config.toml
        // [gateway] port > default) — this used to read `DUDUCLAW_PORT` only
        // and default to a stale 3000 (the gateway's actual default is
        // 18789), so an unconfigured A2A client following this card's `url`
        // with no env var set landed on a dead port.
        "url": format!(
            "http://localhost:{}",
            duduclaw_core::gateway_port_for_home(&duduclaw_core::duduclaw_home()).0
        ),
        "version": crate::updater::current_version(),
        "capabilities": {
            "streaming": true,
            "multi_turn": true,
            "tool_use": true,
        },
        "skills": [
            {"name": "chat", "description": "Multi-turn conversation", "tags": ["conversation"]},
            {"name": "channel_messaging", "description": "Telegram/LINE/Discord messaging", "tags": ["messaging"]},
            {"name": "memory", "description": "Search and store memories", "tags": ["memory"]},
        ],
    })
}

async fn well_known_agent_card() -> axum::Json<serde_json::Value> {
    let mut card = build_agent_card();
    // A2A v1.0 signature — only added when a signer is available; original
    // fields are never modified (only-add invariant). Fail-closed on error =>
    // serve the unsigned card rather than a 500.
    if let Some(signer) = a2a_signer() {
        signer.sign_card(&mut card);
    }
    axum::Json(card)
}

/// JWKS endpoint advertising the A2A signing public key (RFC 8037 OKP/Ed25519).
/// Empty key set when no signer is available.
async fn well_known_jwks() -> axum::Json<serde_json::Value> {
    match a2a_signer() {
        Some(signer) => axum::Json(signer.jwks()),
        None => axum::Json(serde_json::json!({ "keys": [] })),
    }
}

#[cfg(test)]
mod shutdown_sequence_tests {
    //! W3-4 debt: `InferenceEngine::flush_shadow_observations` existed with
    //! zero call sites, so a detached UCCI LocalStrong shadow that was still
    //! generating at exit simply lost its calibration row.

    const SRC: &str = include_str!("server.rs");

    const PREDICTION_FLUSH: &str = "bounded_step(\"prediction engine flush\"";
    const SHADOW_FLUSH: &str = "crate::claude_runner::flush_inference_shadow_observations()";
    // The flush chain hands over to axum's connection drain by firing this
    // oneshot; every state flush has to sit before it. (Until 2026-09-29 the
    // last flush-chain step was the PTY-pool worker-supervisor shutdown; that
    // subsystem was removed in the feature audit — S1-B/D4 — so the drain
    // handover is now the boundary.)
    const DRAIN_HANDOVER: &str = "drain_started_tx.send(())";

    /// Structural, because the shutdown future lives inside `start_server`'s
    /// several-thousand-line body and cannot be driven from a unit test.
    /// Order is the point: flushing before the prediction engine would race
    /// the engine's own state, and flushing after the drain handover would
    /// let the process exit while a calibration row is still being written.
    #[test]
    fn shadow_flush_is_sequenced_between_prediction_flush_and_drain_handover() {
        let prediction = SRC
            .find(PREDICTION_FLUSH)
            .expect("the prediction-engine flush step must still exist");
        let shadow = SRC
            .find(SHADOW_FLUSH)
            .expect("the UCCI shadow flush must be wired into graceful shutdown");
        let handover = SRC
            .find(DRAIN_HANDOVER)
            .expect("the flush chain must still hand over to the connection drain");
        assert!(
            prediction < shadow,
            "the shadow flush must come after the prediction-engine flush"
        );
        assert!(
            shadow < handover,
            "the shadow flush must come before the drain handover"
        );
    }

    /// The step must stay bounded: a wedged local generation cannot be
    /// allowed to hold the process open through a restart.
    #[test]
    fn shadow_flush_is_bounded_and_warn_only() {
        let step = SRC
            .find("bounded_step(\n            \"UCCI shadow observation flush\",\n            5,")
            .or_else(|| SRC.find("bounded_step(\"UCCI shadow observation flush\", 5,"));
        assert!(
            step.is_some(),
            "the shadow flush must go through `bounded_step` with a 5s bound"
        );
    }
}
