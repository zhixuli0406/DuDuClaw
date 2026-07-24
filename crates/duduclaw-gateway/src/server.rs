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
use crate::extension::GatewayExtension;
use crate::handlers::MethodHandler;
use crate::protocol::WsFrame;

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

    let home_dir = config.home_dir.clone();
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
                info!("internal MCP API key active for this gateway (spawned MCP children authenticate)");
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
    {
        let cfg_path = home_dir.join("config.toml");
        let parsed: Option<duduclaw_redaction::RedactionConfig> =
            std::fs::read_to_string(&cfg_path).ok().and_then(|s| {
                #[derive(serde::Deserialize)]
                struct Wrap {
                    #[serde(default)]
                    redaction: duduclaw_redaction::RedactionConfig,
                }
                toml::from_str::<Wrap>(&s).ok().map(|w| w.redaction)
            });

        match parsed {
            Some(rcfg) if rcfg.enabled => {
                match crate::redaction_integration::build_manager_from_home(&home_dir, rcfg.clone())
                {
                    Ok(manager) => {
                        info!(
                            rules = manager.engine().rule_count(),
                            ttl_h = manager.vault_ttl_hours(),
                            "RFC-23 redaction pipeline enabled"
                        );
                        handler.swap_redaction_manager(Some(manager)).await;
                    }
                    Err(e) => {
                        // Fail-closed at startup: if redaction was requested
                        // but cannot be initialised, we surface the failure
                        // loudly. We still continue (no redaction) — operator
                        // must observe and act.
                        warn!(
                            error = %e,
                            "RFC-23 redaction pipeline FAILED to initialise — \
                             gateway continues WITHOUT redaction. Check \
                             config.toml [redaction] and ~/.duduclaw/redaction/."
                        );
                    }
                }
            }
            _ => {
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
    match user_db.ensure_default_admin() {
        Ok(Some(password)) => {
            // First-run bootstrap. The dashboard's first-open screen lets a
            // LOOPBACK operator SET the admin password directly (the
            // `/api/first-run/claim` flow), so on a localhost bind the
            // generated one-time password is a stale second path — printing
            // it just confuses the setup. Only a non-loopback bind (where the
            // loopback-only claim endpoint is unreachable from the operator's
            // browser) still needs the printed value to get in at all.
            let loopback_only = matches!(config.bind.as_str(), "127.0.0.1" | "::1" | "localhost");
            if loopback_only {
                println!(
                    "\n  🔑 First-run setup: open the dashboard and set the admin password there (admin@local)."
                );
                println!();
                let _ = password; // superseded by the dashboard claim flow
            } else {
                println!(
                    "\n  🔑 First-run admin — log in with this, you'll be asked to change it:"
                );
                println!("     Email:    admin@local");
                println!("     Password: {password}");
                println!();
            }
        }
        Ok(None) => {} // Admin already exists
        Err(e) => {
            // C2 fix: fail hard if we can't create admin — don't silently continue
            return Err(duduclaw_core::error::DuDuClawError::Gateway(format!(
                "Failed to initialize user database: {e}"
            )));
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
    // Load encryption key for rollback_diff at rest (reuses existing keyfile)
    let gvu_encryption_key = crate::config_crypto::load_keyfile_public(&home_dir);
    let gvu_loop = Arc::new(crate::gvu::loop_::GvuLoop::with_encryption(
        &gvu_db_path,
        None, // observation_hours — will be set per-agent from config
        None, // max_generations — will be set per-agent from config
        gvu_encryption_key.as_ref(),
    ));
    info!(
        "GVU evolution loop initialized (encryption: {})",
        if gvu_encryption_key.is_some() {
            "enabled"
        } else {
            "disabled"
        }
    );

    // ── BUG-1 fix: schedule ObservationFinalizer (30 min ticks) ───────────
    // Closes expired SOUL.md observation windows (confirmed / rolled_back /
    // extended). Without this, the very first applied SOUL change blocks all
    // subsequent GVU proposals indefinitely.
    {
        let finalizer = Arc::new(
            crate::gvu::observation_finalizer::ObservationFinalizer::new(
                crate::gvu::version_store::VersionStore::with_crypto(
                    &gvu_db_path,
                    gvu_encryption_key.as_ref(),
                ),
                home_dir.join("prediction.db"),
                home_dir.join("feedback.jsonl"),
                home_dir.join("agents"),
                gvu_encryption_key,
            ),
        );
        tokio::spawn(finalizer.run(std::time::Duration::from_secs(1800)));
        info!("ObservationFinalizer scheduled — 30 min interval");
    }

    // Event broadcast channel for pushing real-time updates (e.g. channel status) to dashboard
    let (event_tx, _) = broadcast::channel::<String>(64);
    handler.set_event_tx(event_tx.clone()).await;

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
        .with_redaction_manager(handler.get_redaction_manager().await),
    );
    // Inject reply context into handler for channel hot-start/stop
    handler.set_reply_ctx(reply_ctx.clone()).await;

    // Store background task handles for graceful shutdown (BE-L4)
    let mut bg_handles: Vec<tokio::task::JoinHandle<()>> = Vec::new();

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

    // Start channel bots — per-agent where supported
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

    // P1 (2026-05-09): build the GvuTriggerCtx once and share it across the
    // silence-event consumer and the dispatcher so both code paths fire GVU
    // through the same plumbing (loop / notebook / home dir). Constructed
    // before the silence consumer spawn — see #3.3 in
    // commercial/docs/TODO-runtime-health-fixes-202605.md for context.
    let shared_gvu_ctx = Arc::new(crate::prediction::subagent_prediction::GvuTriggerCtx {
        gvu_loop: gvu_loop.clone(),
        notebook: Some(Arc::new(
            crate::gvu::mistake_notebook::MistakeNotebook::new(&home_dir.join("evolution.db")),
        )),
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
                let p = policy.clone();
                tokio::task::spawn_blocking(move || {
                    let engine = match duduclaw_memory::SqliteMemoryEngine::new(&db_path) {
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
    if let (Some(ap_store), Some(ts)) = (autopilot_store_opt, task_store_opt.clone()) {
        // Capacity 8192: covers a burst of ~4000 events/hr without
        // dropping under a slow DB. Beyond this, `RecvError::Lagged`
        // surfaces in both the error log and the Activity Feed so the
        // drop isn't silent.
        let (ap_tx, ap_rx) =
            tokio::sync::broadcast::channel::<crate::autopilot_engine::AutopilotEvent>(8192);
        handler.set_autopilot_event_tx(ap_tx.clone()).await;

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
            duduclaw_security::audit::append_audit_event(
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
                    bg_handles.push(crate::autopilot_engine::spawn_events_db_poll(
                        bus.clone(),
                        ap_tx.clone(),
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
        if let Some(bus) = events_bus {
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
        let interruptibility =
            Arc::new(crate::interruptibility::InterruptibilityTracker::new());
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

        // Spawn the engine loop
        let engine = crate::autopilot_engine::AutopilotEngine::new(
            home_dir.clone(),
            ap_store,
            ts,
            mq_for_autopilot,
            ap_rx,
        )
        .with_proactive_gate(proactive_gate);
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
    // Default OFF (conservative rollout default — see `dispatch_engine_enabled`).
    // Lease renewal is wired (LeaseRenewalGuard for in-process workers,
    // `tasks_renew` MCP heartbeat for external agents) and reclaim is
    // conservative (expiry + one full unrenewed lease window), so enabling via
    // `[dispatch] enabled = true` / DUDUCLAW_DISPATCH_ENGINE=1 is safe.
    // Synchronous claim/dependency/complete via the MCP task tools work
    // regardless of this flag.
    if crate::dispatch_engine::dispatch_engine_enabled(&home_dir) {
        if let Some(ts) = task_store_opt.clone() {
            let caller = crate::dispatch_engine::GoalAcceptanceCaller {
                home_dir: home_dir.clone(),
            };
            let judge: Arc<dyn crate::dispatch_engine::AcceptanceJudge> =
                Arc::new(crate::dispatch_engine::LlmAcceptanceJudge::new(caller));
            let engine = Arc::new(
                crate::dispatch_engine::DispatchEngine::new(ts.clone(), Some(judge))
                    // WP4 GroundEval: fold `tool_calls.jsonl` evidence into
                    // the goal-mode acceptance judge prompt.
                    .with_home_dir(home_dir.clone()),
            );
            bg_handles.push(tokio::spawn(async move { engine.run().await }));
            info!("Dispatch engine started (durable SQLite派工：殭屍回收 + goal-mode 驗收)");

            // ── P1: autonomous goal loop driver ──────────────────
            // The DispatchEngine only reviews goal-mode completions; it does NOT
            // drive execution. The goal loop driver is the missing outer loop:
            // it dispatches todo/pending goal_mode tasks onto the existing
            // message_queue wake-up rail, re-dispatches judge-rejected tasks with
            // feedback, and owns the hard termination guards. Build + spawn logic
            // lives on the handler so gateway startup and the `system.update_config`
            // hot reload (iteration_cap_simple / dispatch.policy) share one path;
            // the registered handle is abort+respawned on reload.
            handler.respawn_goal_loop_driver().await;
        }
    } else {
        info!(
            "Dispatch engine disabled (預設關；lease 續租已接上，可用 [dispatch] enabled=true 啟用)"
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
        tokio::spawn(async move {
            // First check after 30 seconds (let gateway finish startup)
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            loop {
                match crate::updater::check_update().await {
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
                            }),
                            seq: None,
                            state_version: None,
                        };
                        if let Ok(json) = serde_json::to_string(&event) {
                            let _ = etx.send(json);
                        }

                        if auto_update {
                            // Pro auto-update: download, verify, install, restart
                            info!(
                                latest = %info.latest_version,
                                "Auto-update: downloading v{}...",
                                info.latest_version,
                            );

                            // Audit log
                            duduclaw_security::audit::append_audit_event(
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

                            match crate::updater::apply_update(
                                &info.download_url,
                                &info.checksum_url,
                            )
                            .await
                            {
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

                                    duduclaw_security::audit::append_audit_event(
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

                                    duduclaw_security::audit::append_audit_event(
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
            "Periodic update checker started (every 6h, auto_update={})", auto_update,
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

    // Phase 3 (2026-05-14): cross-platform PTY pool runtime.
    //
    // Initialises the global `duduclaw-cli-runtime` PtyPool used by agents
    // that opt in via `agent.toml [runtime] pty_pool_enabled = true`. The
    // init is unconditional so the routing decision in claude_runner /
    // channel_reply can short-circuit cheaply; agents that don't opt in
    // never trigger a spawn. See
    // `commercial/docs/TODO-cli-pty-pool-worker.md` for the full design.
    crate::pty_runtime::init(home_dir.clone());
    info!("PTY runtime initialised (Phase 3 adapter — opt-in via agent.toml)");

    // Phase 7 (2026-05-15): optionally promote PTY pool to out-of-process
    // worker subprocess. Gated by `[runtime] worker_managed = true` in
    // <home>/config.toml. When the flag is on, `pty_runtime`'s
    // `acquire_and_invoke` switches transports to HTTP+JSON-RPC against
    // the spawned `duduclaw-cli-worker` instead of the in-process pool.
    //
    // Failure here is non-fatal: a startup error keeps the gateway in
    // in-process mode (the existing behaviour) + emits a warn log so
    // operators can see why the subprocess didn't come up.
    // **Round 2 review fix (HIGH-4)**: instead of detaching a
    // separate `tokio::spawn` that races with `axum::serve`'s own
    // ctrl_c, store the supervisor handle so the axum graceful
    // shutdown closure can call `handle.shutdown().await` AFTER
    // prediction-engine flush, BEFORE returning. This sequences
    // SIGTERM → 3s grace → SIGKILL into the main shutdown path
    // instead of racing it.
    let worker_supervisor: Option<crate::worker_supervisor::WorkerSupervisorHandle> =
        match crate::worker_supervisor::spawn_if_enabled(&home_dir).await {
            Ok(Some(handle)) => {
                crate::pty_runtime::set_managed_worker(handle.client());
                info!(
                    bind = %handle.bind(),
                    "Worker supervisor spawned — PTY pool routed through subprocess"
                );
                Some(handle)
            }
            Ok(None) => {
                info!("Worker supervisor disabled ([runtime] worker_managed = false)");
                None
            }
            Err(e) => {
                warn!(
                    error = %e,
                    "Worker supervisor spawn failed — PTY pool stays in-process"
                );
                None
            }
        };

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
        .route("/api/refresh", post(handle_refresh))
        .route("/api/me", get(handle_me))
        .route("/api/change-password", post(handle_change_password))
        .route("/api/first-run/status", get(handle_first_run_status))
        .route("/api/first-run/claim", post(handle_first_run_claim))
        .with_state(state.clone());

    let mut app = Router::new()
        .route("/ws", get(ws_handler))
        .route("/health", get(health_handler))
        .route("/metrics", get(crate::metrics::metrics_handler))
        .route("/api/runtime/status", get(crate::runtime_status::handler))
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

    let app = app;

    let addr = format!("{}:{}", config.bind, config.port);
    info!("Gateway starting on {}", addr);

    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .map_err(|e| duduclaw_core::error::DuDuClawError::Gateway(e.to_string()))?;

    // Serve with graceful shutdown on Ctrl+C.
    //
    // **Round 2 review fix (HIGH-4)**: the worker supervisor's
    // SIGTERM/SIGKILL chain is sequenced INSIDE the shutdown future
    // rather than racing it from a detached task. Order:
    //   ctrl_c → prediction engine flush → supervisor shutdown
    //   (SIGTERM → 3s grace → SIGKILL) → axum drains → main exits.
    let pe_for_shutdown = prediction_engine.clone();
    let meta_path_for_shutdown = metacognition_path.clone();
    let supervisor_for_shutdown = worker_supervisor;
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        let _ = tokio::signal::ctrl_c().await;
        info!("Shutdown signal received, flushing state...");
        pe_for_shutdown.flush_all().await;
        pe_for_shutdown
            .persist_metacognition(&meta_path_for_shutdown)
            .await;
        info!("Prediction engine state flushed");
        if let Some(supervisor) = supervisor_for_shutdown {
            info!("Shutting down worker supervisor...");
            supervisor.shutdown().await;
            info!("Worker supervisor shut down");
        }
    })
    .await
    .map_err(|e| duduclaw_core::error::DuDuClawError::Gateway(e.to_string()))?;

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
    authenticate_jwt(state, token).map(|_| ()).map_err(|_| {
        (
            axum::http::StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({ "error": "invalid or expired token" })),
        )
            .into_response()
    })
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
        .on_upgrade(move |socket| handle_socket(socket, state))
}

/// Process a single WebSocket connection.
async fn handle_socket(mut socket: WebSocket, state: Arc<AppState>) {
    info!("New WebSocket connection established");

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
                    match serde_json::from_str::<WsFrame>(&text) {
                        Ok(WsFrame::Request { id, method, params }) if method == "connect" => {
                            // ── JWT authentication (new) ─────────────────────
                            if let Some(jwt_str) = params.get("jwt").and_then(|v| v.as_str()) {
                                match authenticate_jwt(&state, jwt_str) {
                                    Ok(ctx) => {
                                        let ok = WsFrame::ok_response(
                                            &id,
                                            serde_json::json!({
                                                "status": "authenticated",
                                                "user": {
                                                    "id": ctx.user_id,
                                                    "email": ctx.email,
                                                    "role": ctx.role.to_string(),
                                                }
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

    info!(user = %user_ctx.email, role = %user_ctx.role, "WebSocket authenticated");

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
                                // Track log subscription state
                                if method == "logs.subscribe" {
                                    logs_subscribed = true;
                                } else if method == "logs.unsubscribe" {
                                    logs_subscribed = false;
                                }

                                let mut response = state.handler.handle(&method, params, &user_ctx).await;

                                // P4-3+ OS live event tail: unlike `logs.subscribe` above (which
                                // flips its flag on the method NAME alone, before authorization
                                // runs), gate this flag on the ACTUAL response outcome. os_file/
                                // os_frontmost events can carry filesystem paths and window
                                // titles, so a denied (non-admin) `os.events.subscribe` must never
                                // start the forwarding tail.
                                match method.as_str() {
                                    "os.events.subscribe" => {
                                        if matches!(&response, WsFrame::Response { ok: true, .. }) {
                                            os_events_subscribed = true;
                                            os_window_start_ms = 0;
                                            os_window_count = 0;
                                        }
                                    }
                                    "os.events.unsubscribe" => {
                                        os_events_subscribed = false;
                                    }
                                    _ => {}
                                }

                                if let WsFrame::Response { id: ref mut resp_id, .. } = response {
                                    *resp_id = id;
                                }
                                let resp_text = serde_json::to_string(&response).unwrap_or_default();
                                if sink.send(Message::Text(resp_text.into())).await.is_err() { break; }
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

/// Verify a JWT access token and build a UserContext.
/// Single DB lookup, fail-closed on error (R2 fix for double-lookup + fail-open).
fn authenticate_jwt(state: &AppState, jwt_str: &str) -> Result<UserContext, String> {
    let claims = state.jwt_config.verify_access_token(jwt_str)?;

    // Single DB lookup — fail-closed: DB error = reject
    match state.user_db.get_user(&claims.sub) {
        Ok(Some(user)) if user.status == duduclaw_auth::UserStatus::Active => {
            // C1: block all operations until the bootstrap/forced password is
            // changed. The dedicated POST /api/change-password endpoint does not
            // pass through this gate, so the user can still recover.
            if user.must_change_password {
                return Err("password change required before any operation".to_string());
            }
        }
        Ok(Some(_)) => return Err("account is suspended or offboarded".to_string()),
        Ok(None) => return Err("user not found".to_string()),
        Err(_) => return Err("authentication service unavailable".to_string()),
    }

    UserContext::from_claims(&claims)
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
        "url": format!("http://localhost:{}", std::env::var("DUDUCLAW_PORT").unwrap_or_else(|_| "3000".to_string())),
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
