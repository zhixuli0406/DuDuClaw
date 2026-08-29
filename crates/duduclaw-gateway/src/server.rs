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
    // Boot reference for the /healthz scheduler-staleness probe.
    SERVER_START_UNIX.store(
        chrono::Utc::now().timestamp(),
        std::sync::atomic::Ordering::Relaxed,
    );

    let home_dir = config.home_dir.clone();

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
            println!(
                "\n  🔑 First-run admin — log in with this, you'll be asked to change it:"
            );
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
    // Load encryption key for rollback_diff at rest (reuses existing keyfile)
    let gvu_encryption_key = crate::config_crypto::load_keyfile_public(&home_dir);
    let gvu_loop = Arc::new(
        crate::gvu::loop_::GvuLoop::with_encryption(
            &gvu_db_path,
            None, // observation_hours — will be set per-agent from config
            None, // max_generations — will be set per-agent from config
            gvu_encryption_key.as_ref(),
        )
        // WP0.2: consolidate-mode alerts (SOUL.md cap deadlock cleared, or
        // failed to clear) land in the Activity Feed + evolution events
        // instead of a log line nobody reads.
        .with_alert_sink(crate::gvu::consolidate::GvuAlertSink {
            home_dir: home_dir.clone(),
            prediction_engine: prediction_engine.clone(),
        }),
    );
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

    // ── WP0.5: GVU stagnation detector (30 min ticks, same cadence as
    // ObservationFinalizer) ───────────────────────────────────────────────
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
        let monitor = Arc::new(crate::channel_alerts::ChannelAlertMonitor::new(home_dir.clone()));
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
        let drainer = Arc::new(crate::notify_governance::DeferredNotifyDrainer::new(home_dir.clone()));
        tokio::spawn(drainer.run(std::time::Duration::from_secs(60)));
        info!("Deferred-notification drainer scheduled — 1 min interval");
    }

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
        let digest = Arc::new(crate::notify_digest::DailyDigestScheduler::new(home_dir.clone()));
        tokio::spawn(digest.run(std::time::Duration::from_secs(60)));
        info!("Daily-digest scheduler scheduled — 1 min interval (off unless [notify] daily_digest = true)");
    }

    // Belief loop × goal contract gap 2 (design-market-belief-loop-2026-08.md
    // §3 「自主研究」): sweeps every agent every 5 minutes, self-gating on
    // per-agent `agent.toml [research] self_study` (off by default) — a
    // deployment where no agent opts in pays one `agents/` directory read
    // per tick and creates nothing.
    {
        let self_study = Arc::new(crate::self_study::SelfStudyScheduler::new(home_dir.clone()));
        tokio::spawn(self_study.run(std::time::Duration::from_secs(300)));
        info!("Self-study scheduler scheduled — 5 min interval (off unless an agent sets [research] self_study = true)");
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
        let backup_sched = Arc::new(crate::backup_schedule::BackupScheduler::new(home_dir.clone()));
        tokio::spawn(backup_sched.run(std::time::Duration::from_secs(300)));
        info!("Backup scheduler scheduled — 5 min tick (off unless [backup] schedule_enabled = true)");
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
                info!(sources = handles.len(), "Resident sensing tick sources started");
            }
            bg_handles.extend(handles);
        }

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
        warn!("maintenance mode was open before this restart — force-closed (revoke_reason=gateway_restart)");
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
                            if skip_logged_for.as_deref()
                                != Some(info.latest_version.as_str())
                            {
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

                            let apply_result = match &update_provider {
                                Some(provider) => {
                                    provider.apply(&info, &|_| {}).await
                                }
                                None => crate::updater::apply_update(
                                    &info.download_url,
                                    &info.checksum_url,
                                )
                                .await,
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

    // Phase 3 (2026-05-14): cross-platform PTY pool runtime.
    //
    // Initialises the global `duduclaw-cli-runtime` PtyPool used by agents
    // that opt in via `agent.toml [runtime] pty_pool_enabled = true`. The
    // init is unconditional so the routing decision in claude_runner /
    // channel_reply can short-circuit cheaply; agents that don't opt in
    // never trigger a spawn. See
    // `commercial/docs/TODO-cli-pty-pool-worker.md` for the full design.
    // (The `pty_runtime::init` this describes runs below, after the two
    // one-time migrations that must precede it.)

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

    // WP10 one-time migration (2026-08-04): undo the PTY-pool settings the
    // dashboard's agent edit page wrote WITHOUT user consent between v1.44 and
    // v1.49. Must run BEFORE `pty_runtime::init` so the first reply of this
    // boot already sees the corrected routing. Never blocks boot — the pass
    // swallows its own failures and reports them.
    {
        let report = crate::pty_default_migration::run(&home_dir);
        if !report.reset.is_empty() || !report.failed.is_empty() {
            info!(
                reset = report.reset.len(),
                failed = report.failed.len(),
                "WP10 PTY-default migration applied"
            );
            // The migration changes the user's config without being asked. A
            // silent rewrite is what caused this incident in the first place,
            // so surface it in the dashboard rather than only in the log.
            // Best-effort: `emit` never fails the caller, and the event waits
            // in `events.db` for the 2 s tail if no browser is connected yet.
            crate::dashboard_feedback::emit(
                &home_dir,
                crate::dashboard_feedback::EV_RUNTIME_MIGRATED,
                serde_json::json!({
                    "migration": "wp10-pty-default-reset",
                    "reset_agents": report.reset,
                    "failed_agents": report.failed,
                }),
            )
            .await;
        }
    }

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
        .route("/api/channel-identity/list", get(handle_channel_identity_list))
        .route("/api/refresh", post(handle_refresh))
        .route("/api/me", get(handle_me))
        .route("/api/change-password", post(handle_change_password))
        .route("/api/first-run/status", get(handle_first_run_status))
        .route("/api/first-run/claim", post(handle_first_run_claim))
        // D4a: OOBE pre-auth network setup — see `first_run_network_gate`'s
        // doc for the fail-closed conditions shared by all three routes.
        // Deliberately no `/api/first-run/network/forget` — see
        // `handle_first_run_network_connect`'s doc.
        .route("/api/first-run/network/status", get(handle_first_run_network_status))
        .route("/api/first-run/network/scan", post(handle_first_run_network_scan))
        .route("/api/first-run/network/connect", post(handle_first_run_network_connect))
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
        .route("/api/runtime/status", get(crate::runtime_status::handler))
        // Dashboard file panel (WP1.4): list + download an AI staff member's
        // attachment files. Bearer-JWT gated; download also accepts the JWT as
        // a `token` query param so browser preview/download links work.
        .route("/api/files", get(handle_files_list))
        .route("/api/files/download", get(handle_files_download))
        .route("/api/files/preview", get(handle_files_preview))
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
        .route("/api/device/backups/download", get(handle_device_backup_download))
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
    // **Round 2 review fix (HIGH-4)**: the worker supervisor's
    // SIGTERM/SIGKILL chain is sequenced INSIDE the shutdown future
    // rather than racing it from a detached task. Order:
    //   ctrl_c → prediction engine flush → supervisor shutdown
    //   (SIGTERM → 3s grace → SIGKILL) → axum drains → main exits.
    let pe_for_shutdown = prediction_engine.clone();
    let meta_path_for_shutdown = metacognition_path.clone();
    let supervisor_for_shutdown = worker_supervisor;
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
        if let Some(supervisor) = supervisor_for_shutdown {
            info!("Shutting down worker supervisor...");
            // Internal chain is SIGTERM → 3s grace → SIGKILL; the outer bound
            // only catches a wedged supervisor task.
            bounded_step("worker supervisor shutdown", 15, supervisor.shutdown()).await;
            info!("Worker supervisor shut down");
        }
        // WP10 (2026-08-04 field incident): tear down the IN-PROCESS PTY pool
        // too. The supervisor chain above only covers the out-of-process
        // worker; when `[runtime] worker_managed` is off (or the worker never
        // came up) the pool's interactive `claude` REPL children were orphaned
        // at exit and outlived the restart — so a wedged install stayed wedged
        // and leaked one detached Node process per pooled session.
        bounded_step("pty pool shutdown", 10, crate::pty_runtime::shutdown_pool()).await;
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
    let allowed =
        addr.ip().is_loopback() && state.user_db.is_unclaimed_default_admin() && duduclaw_core::is_appliance();
    if allowed {
        return None;
    }
    Some(
        (
            axum::http::StatusCode::FORBIDDEN,
            Json(serde_json::json!({
                "error": "first-run network setup is only available from localhost on an appliance before setup"
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
        Ok(result) => {
            Json(serde_json::json!({"ok": true, "result": crate::network::scan_result_to_json(&result)}))
                .into_response()
        }
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
        return (axum::http::StatusCode::BAD_REQUEST, Json(serde_json::json!({"error": "ssid must not be empty"})))
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
    duduclaw_security::audit::append_audit_event(
        &state.home_dir,
        &duduclaw_security::audit::AuditEvent::new(
            "wifi_connect",
            &body.ssid,
            duduclaw_security::audit::Severity::Info,
            serde_json::json!({ "ssid": body.ssid, "ok": ok, "code": code, "source": "first_run_oobe" }),
        ),
    );

    match result {
        Ok(()) => {
            Json(serde_json::json!({"ok": true, "result": {"state": "connected", "ssid": body.ssid}}))
                .into_response()
        }
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
    let _ = state
        .user_db
        .log_action(Some(&user.id), "login_local_auto", None, None, Some(&ip_str));

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
    let cache_fresh = match (std::fs::metadata(&cached).and_then(|m| m.modified()), src_mtime) {
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
                && modified.elapsed().map(|d| d.as_secs() > 86_400).unwrap_or(false)
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
                && modified.elapsed().map(|d| d.as_secs() > 86_400).unwrap_or(false)
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
    let (rpc_tx, mut rpc_rx) =
        tokio::sync::mpsc::channel::<(WsFrame, Option<bool>)>(64);

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
/// `docs/todo/TODO-bootstrap-admin-ws-deadlock.md` is unit-testable without a
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
    let name = std::fs::read_to_string(
        duduclaw_core::platform::duduclaw_home().join("config.toml"),
    )
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
            Err(e) => warn!(error = %e, "could not auto-enable google_workspace integration; enable it manually in config.toml"),
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
        pre_auth_handshake_allowed(has_credential, explicitly_requested, ed25519, appliance, loopback)
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
