//! Cost telemetry for tracking Claude API token usage and cache efficiency.
//!
//! Collects per-request token usage from Claude CLI stream-json output,
//! persists to SQLite, and provides cache efficiency analytics.
//!
//! Key metric: `cache_efficiency = cache_read / (input + cache_read + cache_creation)`
//! - < 30%: cache severely broken, consider switching API path
//! - 30-50%: normal but optimizable
//! - > 50%: healthy
//!
//! Reference: <https://cablate.com/articles/reverse-engineer-claude-agent-sdk-hidden-token-cost/>

use duduclaw_llm::{ModelRegistry, NormalizedUsage};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Raw token usage extracted from Claude CLI `stream-json` result events.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
    pub output_tokens: u64,
}

impl TokenUsage {
    /// Cache efficiency ratio: `cache_read / (input + cache_read + cache_creation)`.
    ///
    /// Returns 0.0 when no tokens were consumed.
    pub fn cache_efficiency(&self) -> f64 {
        let total = self.input_tokens + self.cache_read_tokens + self.cache_creation_tokens;
        if total == 0 {
            return 0.0;
        }
        self.cache_read_tokens as f64 / total as f64
    }

    /// Total input tokens (all categories combined).
    pub fn total_input(&self) -> u64 {
        self.input_tokens + self.cache_read_tokens + self.cache_creation_tokens
    }

    /// Estimated cost savings from prompt caching (in millicents).
    ///
    /// Without caching, all cache_read_tokens would be full-price input tokens.
    /// Savings = cache_read_tokens * (full_price - cached_price) per token.
    /// Sonnet 4.6: full=$3/M, cached=$0.30/M → savings = cache_read * $2.70/M
    pub fn cache_savings_millicents(&self) -> u64 {
        // $2.70 per million tokens saved = 270 millicents per million
        // = 0.00027 millicents per token
        (self.cache_read_tokens as f64 * 0.27 / 1000.0) as u64
    }

    /// Whether the total input is approaching the 200K price cliff.
    ///
    /// Anthropic doubles input pricing when input exceeds 200K tokens.
    /// We warn at 180K to allow time for compression.
    ///
    /// Legacy model-agnostic check — prefer [`near_price_cliff_for`], which
    /// derives the threshold from the model registry and only falls back to
    /// the fixed 180K for unknown models.
    pub fn is_near_price_cliff(&self) -> bool {
        self.total_input() > 180_000
    }

    /// Estimated cost for API key usage — LEGACY Sonnet-only fallback.
    ///
    /// Pricing (Sonnet 4.6 baseline):
    /// - Input: $3/M tokens ($6/M above 200K)
    /// - Cache read: $0.30/M tokens
    /// - Cache creation: $3.75/M tokens
    /// - Output: $15/M tokens ($22.50/M above 200K input)
    ///
    /// Unit note: the constants below are cents-per-MTok, so the returned
    /// value (and every stored `cost_millicents` row) has historically been
    /// in CENTS despite the name — `monthly_budget_cents` and the dashboard
    /// (÷100 → dollars) both rely on that scale. [`cost_for`] keeps registry
    /// results on the same scale for comparability.
    ///
    /// Prefer [`cost_for`], which prices known models from the registry and
    /// only routes unknown models here (behavior-compatible fallback).
    pub fn estimated_cost_millicents(&self) -> u64 {
        let above_200k = self.total_input() > 200_000;

        let input_rate = if above_200k { 600 } else { 300 }; // per M tokens, in millicents
        let cache_read_rate = 30; // $0.30/M
        let cache_creation_rate = 375; // $3.75/M
        let output_rate = if above_200k { 2250 } else { 1500 }; // per M tokens

        let cost = |tokens: u64, rate: u64| -> u64 {
            (tokens * rate + 500_000) / 1_000_000 // round to nearest
        };

        cost(self.input_tokens, input_rate)
            + cost(self.cache_read_tokens, cache_read_rate)
            + cost(self.cache_creation_tokens, cache_creation_rate)
            + cost(self.output_tokens, output_rate)
    }

    /// Parse from a serde_json `usage` object in Claude CLI stream-json output.
    pub fn from_json(value: &serde_json::Value) -> Option<Self> {
        Some(Self {
            input_tokens: value.get("input_tokens")?.as_u64()?,
            cache_read_tokens: value
                .get("cache_read_input_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            cache_creation_tokens: value
                .get("cache_creation_input_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            output_tokens: value
                .get("output_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
        })
    }
}

// ---------------------------------------------------------------------------
// Model registry — per-model pricing (duduclaw-llm)
// ---------------------------------------------------------------------------

static MODEL_REGISTRY: std::sync::OnceLock<ModelRegistry> = std::sync::OnceLock::new();

/// Build the pricing registry: vendored seed + optional user override.
fn build_model_registry(override_path: Option<&Path>) -> ModelRegistry {
    let mut registry = ModelRegistry::vendored();
    if let Some(path) = override_path {
        match registry.load_override(path) {
            Ok(0) => {}
            Ok(n) => info!(merged = n, ?path, "model pricing override merged"),
            Err(e) => warn!(
                error = %e, ?path,
                "model pricing override failed to load — using vendored table"
            ),
        }
    }
    registry
}

/// Global model/pricing registry. `init_telemetry` seeds it with the
/// `~/.duduclaw/models.toml` override; if something reads pricing before
/// startup init (tests, early callers) it lazily falls back to the vendored
/// seed only.
pub(crate) fn model_registry() -> &'static ModelRegistry {
    MODEL_REGISTRY.get_or_init(|| build_model_registry(None))
}

/// Map gateway [`TokenUsage`] onto the registry's [`NormalizedUsage`].
///
/// `reasoning_tokens` is 0 by construction: `TokenUsage` has no reasoning
/// field, so multi-provider callers (claude_runner W2 path) fold provider
/// reasoning tokens into `output_tokens` BEFORE recording — reasoning bills
/// at the output rate either way, so the fold is cost-neutral.
fn to_normalized(usage: &TokenUsage) -> NormalizedUsage {
    NormalizedUsage {
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cache_read_tokens: usage.cache_read_tokens,
        cache_write_tokens: usage.cache_creation_tokens,
        reasoning_tokens: 0,
    }
}

/// Cost of `usage` for `model`, in the legacy telemetry unit (cents —
/// see [`TokenUsage::estimated_cost_millicents`] unit note).
///
/// - Model known to the registry → duduclaw-llm cost math (per-model rates,
///   price cliffs, cache read/write pricing). The registry returns true
///   millicents ($1/MTok = 100_000), converted ÷1000 (rounded) to stay on
///   the same scale as historical rows and `monthly_budget_cents`.
/// - Unknown model → legacy hardcoded Sonnet constants, byte-for-byte the
///   pre-registry behavior (debug-logged once per model id).
pub fn cost_for(model: &str, usage: &TokenUsage) -> u64 {
    let registry = model_registry();
    match registry.get(model) {
        Some(info) => {
            let true_millicents = registry.cost_millicents(&to_normalized(usage), info);
            (true_millicents + 500) / 1000
        }
        None => {
            log_unknown_model_once(model);
            usage.estimated_cost_millicents()
        }
    }
}

/// Model-aware price-cliff proximity check.
///
/// - Registry model WITH a cliff → warn above 90% of its threshold
///   (Anthropic/Gemini 200K → 180K, matching the legacy margin).
/// - Registry model WITHOUT a cliff (e.g. DeepSeek) → never near a cliff.
/// - Unknown model → legacy fixed 180K check.
pub fn near_price_cliff_for(model: &str, usage: &TokenUsage) -> bool {
    match model_registry().get(model) {
        Some(info) => match info.price_cliff {
            Some(cliff) => {
                let warn_at = cliff.threshold_tokens - cliff.threshold_tokens / 10;
                usage.total_input() > warn_at
            }
            None => false,
        },
        None => usage.is_near_price_cliff(),
    }
}

/// Debug-log an unknown model id exactly once per process (avoids flooding
/// the log on every request for the same unregistered model).
fn log_unknown_model_once(model: &str) {
    static LOGGED: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> =
        std::sync::OnceLock::new();
    let set = LOGGED.get_or_init(Default::default);
    if let Ok(mut guard) = set.lock() {
        if guard.insert(model.to_string()) {
            debug!(
                model,
                "model not in pricing registry — falling back to legacy Sonnet rates"
            );
        }
    }
}

/// The type of request that triggered the API call.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RequestType {
    Chat,
    Evolution,
    Cron,
    Dispatch,
}

impl RequestType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::Evolution => "evolution",
            Self::Cron => "cron",
            Self::Dispatch => "dispatch",
        }
    }
}

impl std::fmt::Display for RequestType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A single cost record persisted to SQLite.
#[derive(Debug, Clone, Serialize)]
pub struct CostRecord {
    pub agent_id: String,
    pub request_type: String,
    pub model: String,
    pub input_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
    pub output_tokens: u64,
    pub cache_efficiency: f64,
    pub cost_millicents: u64,
    /// Redundant with `cache_efficiency` — kept for SQLite schema compatibility.
    /// Both are computed from `TokenUsage::cache_efficiency()`.
    pub cache_hit_rate: f64,
    /// Estimated savings from prompt caching (millicents).
    pub cache_savings_millicents: u64,
    pub created_at: String,
}

/// Aggregated cost summary for a time window.
#[derive(Debug, Clone, Default, Serialize)]
pub struct CostSummary {
    pub period: String,
    pub total_requests: u64,
    pub total_input_tokens: u64,
    pub total_cache_read_tokens: u64,
    pub total_cache_creation_tokens: u64,
    pub total_output_tokens: u64,
    pub avg_cache_efficiency: f64,
    pub total_cost_millicents: u64,
    /// Redundant with `avg_cache_efficiency` — kept for SQLite schema compatibility.
    /// Both are computed from `TokenUsage::cache_efficiency()`.
    pub avg_cache_hit_rate: f64,
    /// Total estimated savings from prompt caching (millicents).
    pub total_cache_savings_millicents: u64,
}

/// Per-agent summary with cache health indicator.
#[derive(Debug, Clone, Serialize)]
pub struct AgentCostSummary {
    pub agent_id: String,
    pub summary: CostSummary,
    /// "healthy" (>50%), "normal" (30-50%), "degraded" (<30%)
    pub cache_health: String,
}

/// Per-user cost aggregate (WP6). `user_id` is `"(system)"` for non-attributed
/// traffic (sub-agent dispatch, evolution, utility calls).
#[derive(Debug, Clone, Serialize)]
pub struct UserCostSummary {
    pub user_id: String,
    pub total_requests: u64,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_cost_millicents: u64,
}

// ---------------------------------------------------------------------------
// CostTelemetry — SQLite-backed analytics
// ---------------------------------------------------------------------------

/// TTL for the in-memory cost-pressure flag (#6.3, 2026-05-09).
///
/// When an agent crosses the 200 K price-cliff, we mark it as "under
/// cost pressure" so prompt builders can route the next request through
/// the LLMLingua-2 / meta-token compression path (future work — the
/// flag is the foundation observability layer for that). 1 hour gives
/// the agent a window to either self-correct or stay flagged through
/// follow-up turns; longer would risk sticking the flag past the
/// problem.
pub const COST_PRESSURE_TTL: std::time::Duration =
    std::time::Duration::from_secs(3600);

/// Persistent cost telemetry engine backed by SQLite.
///
/// Thread-safe via internal Mutex on the connection.
pub struct CostTelemetry {
    conn: Mutex<Connection>,
    db_path: PathBuf,
    /// Per-agent timestamp of the last 200K price-cliff trip. Used as a
    /// TTL'd "under cost pressure" flag — see `is_under_cost_pressure`.
    /// Sync `std::sync::Mutex` because every reader is fast and we want
    /// to hand the lock back to whoever asks (prompt builders are sync
    /// through their existing call sites).
    cost_pressure_flags:
        std::sync::Mutex<std::collections::HashMap<String, chrono::DateTime<chrono::Utc>>>,
}

impl CostTelemetry {
    /// Open (or create) the telemetry database and initialize the schema.
    pub fn new(db_path: &Path) -> Result<Self, String> {
        let conn =
            Connection::open(db_path).map_err(|e| format!("open telemetry db: {e}"))?;

        Self::init_schema(&conn)?;

        info!(?db_path, "CostTelemetry initialized");
        Ok(Self {
            conn: Mutex::new(conn),
            db_path: db_path.to_path_buf(),
            cost_pressure_flags: std::sync::Mutex::new(
                std::collections::HashMap::new(),
            ),
        })
    }

    /// Mark `agent_id` as currently under cost pressure. Called from
    /// [`Self::record`] when a request trips the 200 K price cliff so
    /// prompt builders know to engage compression next turn.
    fn mark_cost_pressure(&self, agent_id: &str) {
        if let Ok(mut guard) = self.cost_pressure_flags.lock() {
            guard.insert(agent_id.to_string(), chrono::Utc::now());
        }
    }

    /// `true` when `agent_id` has tripped the 200K cliff within
    /// `COST_PRESSURE_TTL`. Stale entries are pruned lazily on read so
    /// the map can't grow unbounded. Used by prompt builders (future
    /// step) to route through prompt compression.
    pub fn is_under_cost_pressure(&self, agent_id: &str) -> bool {
        let mut guard = match self.cost_pressure_flags.lock() {
            Ok(g) => g,
            Err(_) => return false,
        };
        let now = chrono::Utc::now();
        let cutoff = now - chrono::Duration::from_std(COST_PRESSURE_TTL).unwrap();
        // Lazy purge — bounded by the per-agent count, not by call rate.
        guard.retain(|_, ts| *ts >= cutoff);
        guard.contains_key(agent_id)
    }

    fn init_schema(conn: &Connection) -> Result<(), String> {
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
            PRAGMA busy_timeout=5000;

            CREATE TABLE IF NOT EXISTS token_usage (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                agent_id TEXT NOT NULL,
                request_type TEXT NOT NULL,
                model TEXT NOT NULL DEFAULT '',
                input_tokens INTEGER NOT NULL DEFAULT 0,
                cache_read_tokens INTEGER NOT NULL DEFAULT 0,
                cache_creation_tokens INTEGER NOT NULL DEFAULT 0,
                output_tokens INTEGER NOT NULL DEFAULT 0,
                cache_efficiency REAL NOT NULL DEFAULT 0.0,
                cost_millicents INTEGER NOT NULL DEFAULT 0,
                cache_hit_rate REAL NOT NULL DEFAULT 0.0,
                cache_savings_millicents INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_token_usage_agent_time
                ON token_usage(agent_id, created_at);

            CREATE INDEX IF NOT EXISTS idx_token_usage_time
                ON token_usage(created_at);",
        )
        .map_err(|e| format!("init telemetry schema: {e}"))?;

        // WP6 additive migration: per-user / per-channel attribution columns.
        // Idempotent — an "duplicate column name" error on an already-migrated
        // DB is expected and ignored (same pattern as the memory crate).
        for stmt in [
            "ALTER TABLE token_usage ADD COLUMN user_id TEXT",
            "ALTER TABLE token_usage ADD COLUMN channel TEXT",
        ] {
            if let Err(e) = conn.execute(stmt, []) {
                let msg = e.to_string();
                if !msg.contains("duplicate column name") {
                    return Err(format!("token_usage migration failed: {e}"));
                }
            }
        }
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_token_usage_user_time
                ON token_usage(user_id, created_at);",
        )
        .map_err(|e| format!("token_usage user index: {e}"))?;
        Ok(())
    }

    /// Record a single API call's token usage (no user/channel attribution).
    /// Thin wrapper over [`Self::record_attributed`] for callers that don't have
    /// a human user (sub-agent dispatch, evolution, utility calls).
    pub async fn record(
        &self,
        agent_id: &str,
        request_type: RequestType,
        model: &str,
        usage: &TokenUsage,
    ) {
        self.record_attributed(agent_id, request_type, model, usage, None, None)
            .await;
    }

    /// Record a single API call's token usage, attributing it to a specific end
    /// user and channel where known (WP6 — "which employee spent this?").
    /// `user_id` / `channel` are optional so non-channel callers keep working.
    pub async fn record_attributed(
        &self,
        agent_id: &str,
        request_type: RequestType,
        model: &str,
        usage: &TokenUsage,
        user_id: Option<&str>,
        channel: Option<&str>,
    ) {
        let now = chrono::Utc::now().to_rfc3339();
        let efficiency = usage.cache_efficiency();
        let cost = cost_for(model, usage);
        let cache_hit_rate = usage.cache_efficiency();
        let cache_savings = usage.cache_savings_millicents();

        let conn = self.conn.lock().await;
        let result = conn.execute(
            "INSERT INTO token_usage
             (agent_id, request_type, model, input_tokens, cache_read_tokens,
              cache_creation_tokens, output_tokens, cache_efficiency, cost_millicents,
              cache_hit_rate, cache_savings_millicents, created_at, user_id, channel)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![
                agent_id,
                request_type.as_str(),
                model,
                usage.input_tokens,
                usage.cache_read_tokens,
                usage.cache_creation_tokens,
                usage.output_tokens,
                efficiency,
                cost,
                cache_hit_rate,
                cache_savings,
                now,
                user_id,
                channel,
            ],
        );

        if let Err(e) = result {
            warn!(error = %e, "Failed to record token usage");
            return;
        }

        // Log warnings for anomalies
        if cache_hit_rate < 0.3 && usage.total_input() > 1000 {
            warn!(
                agent_id,
                cache_hit_rate = %format!("{:.1}%", cache_hit_rate * 100.0),
                cache_creation = usage.cache_creation_tokens,
                "Low cache hit rate — consider stabilizing system prompt prefix"
            );
        }

        if near_price_cliff_for(model, usage) {
            warn!(
                agent_id,
                model,
                total_input = usage.total_input(),
                "Approaching long-context price cliff — input pricing will increase"
            );
            // #6.3 (2026-05-09): turn the warn into an actionable event.
            // Mark the agent for compression-mode routing on the next
            // request, and persist a `cost_pressure` row to evolution.db
            // so dashboards can plot pressure history alongside silence
            // breakers / GVU triggers.
            self.mark_cost_pressure(agent_id);
            spawn_evolution_event_write(
                &self.db_path,
                agent_id.to_string(),
                usage.total_input(),
                request_type.as_str().to_string(),
            );
        }

        info!(
            agent_id,
            request_type = request_type.as_str(),
            input = usage.input_tokens,
            cache_read = usage.cache_read_tokens,
            cache_write = usage.cache_creation_tokens,
            output = usage.output_tokens,
            cache_eff = format!("{:.1}%", efficiency * 100.0),
            cost_mc = cost,
            cache_savings_mc = cache_savings,
            "Token usage recorded"
        );
    }

    /// Global cost summary for a time window.
    ///
    /// `hours_ago`: how far back to look (e.g., 1 = last hour, 24 = last day).
    pub async fn summary_global(&self, hours_ago: u64) -> Result<CostSummary, String> {
        let cutoff = cutoff_time(hours_ago);
        let period = format!("last_{hours_ago}h");
        let conn = self.conn.lock().await;

        conn.query_row(
            "SELECT
                COUNT(*),
                COALESCE(SUM(input_tokens), 0),
                COALESCE(SUM(cache_read_tokens), 0),
                COALESCE(SUM(cache_creation_tokens), 0),
                COALESCE(SUM(output_tokens), 0),
                COALESCE(AVG(cache_efficiency), 0.0),
                COALESCE(SUM(cost_millicents), 0),
                COALESCE(AVG(cache_hit_rate), 0.0),
                COALESCE(SUM(cache_savings_millicents), 0)
             FROM token_usage
             WHERE created_at >= ?1",
            params![cutoff],
            |row| {
                Ok(CostSummary {
                    period,
                    total_requests: safe_u64(row.get::<_, i64>(0)?),
                    total_input_tokens: safe_u64(row.get::<_, i64>(1)?),
                    total_cache_read_tokens: safe_u64(row.get::<_, i64>(2)?),
                    total_cache_creation_tokens: safe_u64(row.get::<_, i64>(3)?),
                    total_output_tokens: safe_u64(row.get::<_, i64>(4)?),
                    avg_cache_efficiency: row.get(5)?,
                    total_cost_millicents: safe_u64(row.get::<_, i64>(6)?),
                    avg_cache_hit_rate: row.get(7)?,
                    total_cache_savings_millicents: safe_u64(row.get::<_, i64>(8)?),
                })
            },
        )
        .map_err(|e| format!("summary_global: {e}"))
    }

    /// Per-agent cost summary for a time window.
    pub async fn summary_by_agent(
        &self,
        agent_id: &str,
        hours_ago: u64,
    ) -> Result<AgentCostSummary, String> {
        let cutoff = cutoff_time(hours_ago);
        let period = format!("last_{hours_ago}h");
        let conn = self.conn.lock().await;

        let summary = conn
            .query_row(
                "SELECT
                    COUNT(*),
                    COALESCE(SUM(input_tokens), 0),
                    COALESCE(SUM(cache_read_tokens), 0),
                    COALESCE(SUM(cache_creation_tokens), 0),
                    COALESCE(SUM(output_tokens), 0),
                    COALESCE(AVG(cache_efficiency), 0.0),
                    COALESCE(SUM(cost_millicents), 0),
                    COALESCE(AVG(cache_hit_rate), 0.0),
                    COALESCE(SUM(cache_savings_millicents), 0)
                 FROM token_usage
                 WHERE agent_id = ?1 AND created_at >= ?2",
                params![agent_id, cutoff],
                |row| {
                    Ok(CostSummary {
                        period,
                        total_requests: safe_u64(row.get::<_, i64>(0)?),
                        total_input_tokens: safe_u64(row.get::<_, i64>(1)?),
                        total_cache_read_tokens: safe_u64(row.get::<_, i64>(2)?),
                        total_cache_creation_tokens: safe_u64(row.get::<_, i64>(3)?),
                        total_output_tokens: safe_u64(row.get::<_, i64>(4)?),
                        avg_cache_efficiency: row.get(5)?,
                        total_cost_millicents: safe_u64(row.get::<_, i64>(6)?),
                        avg_cache_hit_rate: row.get(7)?,
                        total_cache_savings_millicents: safe_u64(row.get::<_, i64>(8)?),
                    })
                },
            )
            .map_err(|e| format!("summary_by_agent: {e}"))?;

        let cache_health = if summary.avg_cache_efficiency > 0.5 {
            "healthy"
        } else if summary.avg_cache_efficiency > 0.3 {
            "normal"
        } else {
            "degraded"
        }
        .to_string();

        Ok(AgentCostSummary {
            agent_id: agent_id.to_string(),
            summary,
            cache_health,
        })
    }

    /// List all agents with their cost summaries, sorted by total cost descending.
    pub async fn all_agents_summary(
        &self,
        hours_ago: u64,
    ) -> Result<Vec<AgentCostSummary>, String> {
        let cutoff = cutoff_time(hours_ago);
        let period = format!("last_{hours_ago}h");
        let conn = self.conn.lock().await;

        let mut stmt = conn
            .prepare(
                "SELECT
                    agent_id,
                    COUNT(*),
                    COALESCE(SUM(input_tokens), 0),
                    COALESCE(SUM(cache_read_tokens), 0),
                    COALESCE(SUM(cache_creation_tokens), 0),
                    COALESCE(SUM(output_tokens), 0),
                    COALESCE(AVG(cache_efficiency), 0.0),
                    COALESCE(SUM(cost_millicents), 0),
                    COALESCE(AVG(cache_hit_rate), 0.0),
                    COALESCE(SUM(cache_savings_millicents), 0)
                 FROM token_usage
                 WHERE created_at >= ?1
                 GROUP BY agent_id
                 ORDER BY SUM(cost_millicents) DESC",
            )
            .map_err(|e| format!("all_agents_summary prepare: {e}"))?;

        let rows = stmt
            .query_map(params![cutoff], |row| {
                let agent_id: String = row.get(0)?;
                let avg_eff: f64 = row.get(6)?;
                let cache_health = if avg_eff > 0.5 {
                    "healthy"
                } else if avg_eff > 0.3 {
                    "normal"
                } else {
                    "degraded"
                }
                .to_string();

                Ok(AgentCostSummary {
                    agent_id,
                    summary: CostSummary {
                        period: period.clone(),
                        total_requests: safe_u64(row.get::<_, i64>(1)?),
                        total_input_tokens: safe_u64(row.get::<_, i64>(2)?),
                        total_cache_read_tokens: safe_u64(row.get::<_, i64>(3)?),
                        total_cache_creation_tokens: safe_u64(row.get::<_, i64>(4)?),
                        // L10: clamp via safe_u64 like the sibling fields — a raw
                        // `as u64` cast of a negative i64 wraps to a huge value.
                        total_output_tokens: safe_u64(row.get::<_, i64>(5)?),
                        avg_cache_efficiency: avg_eff,
                        total_cost_millicents: safe_u64(row.get::<_, i64>(7)?),
                        avg_cache_hit_rate: row.get(8)?,
                        total_cache_savings_millicents: safe_u64(row.get::<_, i64>(9)?),
                    },
                    cache_health,
                })
            })
            .map_err(|e| format!("all_agents_summary query: {e}"))?;

        let mut result = Vec::new();
        for row in rows {
            result.push(row.map_err(|e| format!("all_agents_summary row: {e}"))?);
        }
        Ok(result)
    }

    /// Per-user cost summary over a time window (WP6 — the boss's "which
    /// employee is spending?" view). Rows with a NULL `user_id` (non-channel
    /// traffic: sub-agent dispatch, evolution) are bucketed under `"(system)"`.
    /// Sorted by total cost descending.
    pub async fn summary_by_user(&self, hours_ago: u64) -> Result<Vec<UserCostSummary>, String> {
        let cutoff = cutoff_time(hours_ago);
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT
                    COALESCE(user_id, '(system)') AS uid,
                    COUNT(*),
                    COALESCE(SUM(input_tokens + cache_read_tokens + cache_creation_tokens), 0),
                    COALESCE(SUM(output_tokens), 0),
                    COALESCE(SUM(cost_millicents), 0)
                 FROM token_usage
                 WHERE created_at >= ?1
                 GROUP BY uid
                 ORDER BY SUM(cost_millicents) DESC",
            )
            .map_err(|e| format!("summary_by_user prepare: {e}"))?;
        let rows = stmt
            .query_map(params![cutoff], |row| {
                Ok(UserCostSummary {
                    user_id: row.get(0)?,
                    total_requests: safe_u64(row.get::<_, i64>(1)?),
                    total_input_tokens: safe_u64(row.get::<_, i64>(2)?),
                    total_output_tokens: safe_u64(row.get::<_, i64>(3)?),
                    total_cost_millicents: safe_u64(row.get::<_, i64>(4)?),
                })
            })
            .map_err(|e| format!("summary_by_user query: {e}"))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| format!("summary_by_user row: {e}"))?);
        }
        Ok(out)
    }

    /// Recent cost records (for debugging / dashboard).
    pub async fn recent_records(&self, limit: u32) -> Result<Vec<CostRecord>, String> {
        let conn = self.conn.lock().await;

        let mut stmt = conn
            .prepare(
                "SELECT agent_id, request_type, model, input_tokens, cache_read_tokens,
                        cache_creation_tokens, output_tokens, cache_efficiency,
                        cost_millicents, cache_hit_rate, cache_savings_millicents, created_at
                 FROM token_usage
                 ORDER BY id DESC
                 LIMIT ?1",
            )
            .map_err(|e| format!("recent_records prepare: {e}"))?;

        let rows = stmt
            .query_map(params![limit], |row| {
                Ok(CostRecord {
                    agent_id: row.get(0)?,
                    request_type: row.get(1)?,
                    model: row.get(2)?,
                    input_tokens: safe_u64(row.get::<_, i64>(3)?),
                    cache_read_tokens: safe_u64(row.get::<_, i64>(4)?),
                    cache_creation_tokens: row.get::<_, i64>(5)? as u64,
                    output_tokens: safe_u64(row.get::<_, i64>(6)?),
                    cache_efficiency: row.get(7)?,
                    cost_millicents: safe_u64(row.get::<_, i64>(8)?),
                    cache_hit_rate: row.get(9)?,
                    cache_savings_millicents: safe_u64(row.get::<_, i64>(10)?),
                    created_at: row.get(11)?,
                })
            })
            .map_err(|e| format!("recent_records query: {e}"))?;

        let mut result = Vec::new();
        for row in rows {
            result.push(row.map_err(|e| format!("recent_records row: {e}"))?);
        }
        Ok(result)
    }

    /// Per-day total spend (millicents) for `agent_id` over the last `days`
    /// days, oldest first. Days with no spend are omitted (the returned series
    /// is the set of active days); callers treating it as a baseline should use
    /// the values, not calendar positions. Used by burn-rate anomaly detection.
    pub async fn daily_cost_millicents(
        &self,
        agent_id: &str,
        days: u64,
    ) -> Result<Vec<u64>, String> {
        let cutoff = (chrono::Utc::now() - chrono::Duration::days(days as i64)).to_rfc3339();
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT substr(created_at, 1, 10) AS day,
                        COALESCE(SUM(cost_millicents), 0)
                 FROM token_usage
                 WHERE agent_id = ?1 AND created_at >= ?2
                 GROUP BY day
                 ORDER BY day ASC",
            )
            .map_err(|e| format!("daily_cost prepare: {e}"))?;
        let rows = stmt
            .query_map(params![agent_id, cutoff], |row| {
                Ok(safe_u64(row.get::<_, i64>(1)?))
            })
            .map_err(|e| format!("daily_cost query: {e}"))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| format!("daily_cost row: {e}"))?);
        }
        Ok(out)
    }

    /// Clean up records older than `days` days.
    pub async fn cleanup_old_records(&self, days: u64) -> Result<u64, String> {
        let cutoff =
            (chrono::Utc::now() - chrono::Duration::days(days as i64)).to_rfc3339();
        let conn = self.conn.lock().await;

        let deleted = conn
            .execute(
                "DELETE FROM token_usage WHERE created_at < ?1",
                params![cutoff],
            )
            .map_err(|e| format!("cleanup: {e}"))?;

        if deleted > 0 {
            info!(deleted, days, "Cleaned up old cost telemetry records");
        }
        Ok(deleted as u64)
    }
}

// ---------------------------------------------------------------------------
// Global singleton
// ---------------------------------------------------------------------------

static TELEMETRY: std::sync::OnceLock<CostTelemetry> = std::sync::OnceLock::new();

/// Initialize the global telemetry singleton. Call once at startup.
pub fn init_telemetry(home_dir: &Path) -> Result<(), String> {
    // Seed the pricing registry with the user override while home_dir is in
    // hand. Ignore "already set" — a lazy vendored-only init may have run
    // first, and the OnceLock keeps whichever won (both share the seed).
    let _ = MODEL_REGISTRY.set(build_model_registry(Some(&home_dir.join("models.toml"))));

    let db_path = home_dir.join("cost_telemetry.db");
    let telemetry = CostTelemetry::new(&db_path)?;
    TELEMETRY
        .set(telemetry)
        .map_err(|_| "CostTelemetry already initialized".to_string())
}

/// Get the global telemetry instance (returns None if not initialized).
pub fn get_telemetry() -> Option<&'static CostTelemetry> {
    TELEMETRY.get()
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn cutoff_time(hours_ago: u64) -> String {
    let clamped = hours_ago.min(8760) as i64; // cap at 1 year
    (chrono::Utc::now() - chrono::Duration::hours(clamped)).to_rfc3339()
}

/// Resolve `prediction.db` from the telemetry db path. The two databases
/// share `~/.duduclaw/` as a parent so we just swap the file name. Pure
/// function so the test below can lock the resolution rule.
fn resolve_prediction_db_path(telemetry_db: &Path) -> PathBuf {
    telemetry_db
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("prediction.db")
}

/// Append a `cost_pressure` row to `prediction.db.evolution_events`
/// without holding the cost-telemetry mutex. Background-only so the
/// hot path stays fast even on slow disks.
///
/// Schema mirrors what `PredictionEngine::log_evolution_event` writes
/// so dashboards can union the two sources without special-casing.
fn spawn_evolution_event_write(
    telemetry_db: &Path,
    agent_id: String,
    total_input: u64,
    request_type: String,
) {
    let prediction_db = resolve_prediction_db_path(telemetry_db);
    let event_id = uuid::Uuid::new_v4().to_string();
    let ts = chrono::Utc::now().to_rfc3339();
    let trigger_ctx = format!(
        "[cost_pressure] total_input={total_input} request_type={request_type} \
         threshold=180000 (anthropic 200K cliff doubles input pricing)"
    );
    tokio::task::spawn_blocking(move || {
        let conn = match rusqlite::Connection::open(&prediction_db) {
            Ok(c) => c,
            Err(e) => {
                warn!(?prediction_db, error = %e, "cost_pressure event write skipped — open failed");
                return;
            }
        };
        // Best-effort INSERT — the table is owned by PredictionEngine and
        // already exists by the time the gateway starts taking traffic.
        if let Err(e) = conn.execute(
            "INSERT INTO evolution_events
             (event_id, agent_id, event_type, composite_error, error_category,
              trigger_context, version_id, rollback_reason, timestamp)
             VALUES (?1, ?2, 'cost_pressure', NULL, 'CostCliff', ?3, NULL, NULL, ?4)",
            params![event_id, agent_id, trigger_ctx, ts],
        ) {
            warn!(error = %e, "cost_pressure event insert failed");
        }
    });
}

/// Safely convert SQLite i64 to u64 — clamp negatives to 0.
fn safe_u64(val: i64) -> u64 {
    val.max(0) as u64
}

/// Runtime adaptive routing overrides — agents with degraded cache get prefer_local=true.
///
/// Stored in-memory (not persisted) — resets on restart. Agents with cache
/// efficiency < 30% over the last hour are automatically routed to local inference
/// when possible.
static ADAPTIVE_OVERRIDES: std::sync::OnceLock<tokio::sync::RwLock<std::collections::HashSet<String>>> =
    std::sync::OnceLock::new();

fn overrides_set() -> &'static tokio::sync::RwLock<std::collections::HashSet<String>> {
    ADAPTIVE_OVERRIDES.get_or_init(|| tokio::sync::RwLock::new(std::collections::HashSet::new()))
}

/// Check whether an agent has been adaptively overridden to prefer local inference.
pub async fn should_prefer_local(agent_id: &str) -> bool {
    overrides_set().read().await.contains(agent_id)
}

/// Hourly check: evaluate per-agent cache efficiency and toggle local preference.
///
/// - cache_efficiency < 30% (at least 3 requests) → override prefer_local = true
/// - cache_efficiency > 70% → remove override (cache is working fine)
pub async fn adaptive_routing_check(home_dir: &std::path::Path) {
    let telemetry = match get_telemetry() {
        Some(t) => t,
        None => {
            // Try to initialize if not yet done
            let _ = init_telemetry(home_dir);
            match get_telemetry() {
                Some(t) => t,
                None => return,
            }
        }
    };

    let agents = match telemetry.all_agents_summary(1).await {
        Ok(a) => a,
        Err(e) => {
            warn!(error = %e, "Adaptive routing check failed");
            return;
        }
    };

    let mut overrides = overrides_set().write().await;
    let mut changes = Vec::new();

    for agent in &agents {
        let eff = agent.summary.avg_cache_efficiency;
        let requests = agent.summary.total_requests;

        if requests >= 3 && eff < 0.3 {
            // Degraded cache — prefer local
            if overrides.insert(agent.agent_id.clone()) {
                changes.push(format!(
                    "{}: cache_eff={:.0}% → prefer_local ON",
                    agent.agent_id, eff * 100.0
                ));
            }
        } else if eff > 0.7 {
            // Healthy cache — remove override
            if overrides.remove(&agent.agent_id) {
                changes.push(format!(
                    "{}: cache_eff={:.0}% → prefer_local OFF (cache healthy)",
                    agent.agent_id, eff * 100.0
                ));
            }
        }
    }

    if !changes.is_empty() {
        info!(
            changes = ?changes,
            active_overrides = overrides.len(),
            "Adaptive routing update"
        );
    }

    // Also clean up old telemetry records (keep 30 days)
    if let Err(e) = telemetry.cleanup_old_records(30).await {
        warn!(error = %e, "Telemetry cleanup failed");
    }
}

/// Estimate token count for a string (CJK-aware).
///
/// Rough heuristic: CJK characters ≈ 1.5 tokens each, ASCII ≈ 0.25 tokens per char.
pub fn estimate_tokens(text: &str) -> u64 {
    let mut tokens: f64 = 0.0;
    for ch in text.chars() {
        if ch > '\u{2E80}' {
            // CJK range (approx)
            tokens += 1.5;
        } else {
            tokens += 0.25;
        }
    }
    tokens.ceil() as u64
}

/// Check whether estimated input tokens are near the 200K price cliff.
pub fn check_price_cliff(system_prompt: &str, user_prompt: &str) -> Option<u64> {
    let estimated = estimate_tokens(system_prompt) + estimate_tokens(user_prompt);
    if estimated > 180_000 {
        Some(estimated)
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cache_efficiency_zero_tokens() {
        let usage = TokenUsage::default();
        assert_eq!(usage.cache_efficiency(), 0.0);
    }

    /// L10: a negative i64 must clamp to 0, not wrap to a huge u64.
    #[test]
    fn test_safe_u64_clamps_negative() {
        assert_eq!(safe_u64(-1), 0);
        assert_eq!(safe_u64(i64::MIN), 0);
        assert_eq!(safe_u64(0), 0);
        assert_eq!(safe_u64(42), 42);
        assert_eq!(safe_u64(i64::MAX), i64::MAX as u64);
    }

    #[test]
    fn test_cache_efficiency_all_cache_read() {
        let usage = TokenUsage {
            input_tokens: 0,
            cache_read_tokens: 1000,
            cache_creation_tokens: 0,
            output_tokens: 500,
        };
        assert_eq!(usage.cache_efficiency(), 1.0);
    }

    #[test]
    fn test_cache_efficiency_typical_v1() {
        // V1 pattern: ~15K cached system prompt, ~45K cache write
        let usage = TokenUsage {
            input_tokens: 0,
            cache_read_tokens: 15_000,
            cache_creation_tokens: 45_000,
            output_tokens: 1_200,
        };
        assert_eq!(usage.cache_efficiency(), 0.25);
    }

    #[test]
    fn test_cache_efficiency_typical_v2() {
        // V2 pattern after 4+ messages
        let usage = TokenUsage {
            input_tokens: 0,
            cache_read_tokens: 400_000,
            cache_creation_tokens: 78_000,
            output_tokens: 2_000,
        };
        let eff = usage.cache_efficiency();
        assert!(eff > 0.83 && eff < 0.84, "expected ~0.836, got {eff}");
    }

    #[test]
    fn test_price_cliff_detection() {
        let usage = TokenUsage {
            input_tokens: 150_000,
            cache_read_tokens: 35_000,
            cache_creation_tokens: 0,
            output_tokens: 0,
        };
        assert!(usage.is_near_price_cliff());

        let usage_safe = TokenUsage {
            input_tokens: 100_000,
            cache_read_tokens: 50_000,
            cache_creation_tokens: 0,
            output_tokens: 0,
        };
        assert!(!usage_safe.is_near_price_cliff());
    }

    #[test]
    fn test_parse_from_json() {
        let json = serde_json::json!({
            "input_tokens": 15294,
            "cache_read_input_tokens": 45724,
            "cache_creation_input_tokens": 0,
            "output_tokens": 1203,
        });
        let usage = TokenUsage::from_json(&json).unwrap();
        assert_eq!(usage.input_tokens, 15294);
        assert_eq!(usage.cache_read_tokens, 45724);
        assert_eq!(usage.cache_creation_tokens, 0);
        assert_eq!(usage.output_tokens, 1203);
    }

    #[test]
    fn test_estimate_tokens_ascii() {
        let text = "Hello, world!"; // 13 chars * 0.25 = 3.25 → 4
        assert_eq!(estimate_tokens(text), 4);
    }

    #[test]
    fn test_estimate_tokens_cjk() {
        let text = "你好世界"; // 4 CJK * 1.5 = 6
        assert_eq!(estimate_tokens(text), 6);
    }

    #[test]
    fn test_check_price_cliff() {
        // Short text — no cliff
        assert!(check_price_cliff("short", "prompt").is_none());
    }

    #[tokio::test]
    async fn test_sqlite_crud() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test_telemetry.db");
        let telemetry = CostTelemetry::new(&db_path).unwrap();

        let usage = TokenUsage {
            input_tokens: 15000,
            cache_read_tokens: 45000,
            cache_creation_tokens: 0,
            output_tokens: 1200,
        };

        // Record
        telemetry
            .record("agent_alpha", RequestType::Chat, "claude-sonnet-4-6", &usage)
            .await;

        // Query global summary
        let summary = telemetry.summary_global(1).await.unwrap();
        assert_eq!(summary.total_requests, 1);
        assert_eq!(summary.total_input_tokens, 15000);
        assert_eq!(summary.total_cache_read_tokens, 45000);

        // Query per-agent
        let agent_summary = telemetry.summary_by_agent("agent_alpha", 1).await.unwrap();
        assert_eq!(agent_summary.agent_id, "agent_alpha");
        assert_eq!(agent_summary.cache_health, "healthy"); // 75% > 50%

        // Recent records
        let records = telemetry.recent_records(10).await.unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].agent_id, "agent_alpha");

        // All agents
        let all = telemetry.all_agents_summary(1).await.unwrap();
        assert_eq!(all.len(), 1);
    }

    #[tokio::test]
    async fn wp6_per_user_attribution() {
        let dir = tempfile::tempdir().unwrap();
        let telemetry = CostTelemetry::new(&dir.path().join("u.db")).unwrap();
        let usage = TokenUsage {
            input_tokens: 1000,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            output_tokens: 100,
        };
        // Two users on Telegram + one unattributed (system) record.
        telemetry
            .record_attributed("agnes", RequestType::Chat, "claude-sonnet-4-6", &usage, Some("u-alice"), Some("telegram:1"))
            .await;
        telemetry
            .record_attributed("agnes", RequestType::Chat, "claude-sonnet-4-6", &usage, Some("u-alice"), Some("telegram:1"))
            .await;
        telemetry
            .record_attributed("agnes", RequestType::Chat, "claude-sonnet-4-6", &usage, Some("u-bob"), Some("telegram:2"))
            .await;
        telemetry
            .record("agnes", RequestType::Evolution, "claude-sonnet-4-6", &usage) // no user
            .await;

        let by_user = telemetry.summary_by_user(1).await.unwrap();
        // Alice (2), Bob (1), (system) (1).
        let alice = by_user.iter().find(|u| u.user_id == "u-alice").unwrap();
        assert_eq!(alice.total_requests, 2);
        let bob = by_user.iter().find(|u| u.user_id == "u-bob").unwrap();
        assert_eq!(bob.total_requests, 1);
        assert!(by_user.iter().any(|u| u.user_id == "(system)"));
    }

    #[tokio::test]
    async fn wp6_migration_is_idempotent() {
        // Opening the same DB twice must not fail on the additive ALTERs.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mig.db");
        let _t1 = CostTelemetry::new(&path).unwrap();
        drop(_t1);
        let t2 = CostTelemetry::new(&path).unwrap();
        // And a plain record still works post-migration.
        let usage = TokenUsage { input_tokens: 10, cache_read_tokens: 0, cache_creation_tokens: 0, output_tokens: 1 };
        t2.record("a", RequestType::Chat, "claude-sonnet-4-6", &usage).await;
        assert_eq!(t2.summary_global(1).await.unwrap().total_requests, 1);
    }

    // ── W1: registry-based per-model pricing ────────────────────────────────

    /// Known Anthropic model: registry math must land on the same value as
    /// the legacy Sonnet constants for sub-cliff usage (rates are identical;
    /// per-component vs. total rounding can differ by ≤1 on non-round token
    /// counts, so the fixture uses round numbers).
    #[test]
    fn cost_for_known_anthropic_matches_legacy_sub_cliff() {
        let usage = TokenUsage {
            input_tokens: 100_000,       // $0.30 → 30
            cache_read_tokens: 50_000,   // $0.015 → 1.5
            cache_creation_tokens: 10_000, // $0.0375 → 3.75
            output_tokens: 100_000,      // $1.50 → 150
        };
        // total_input = 160K < 200K cliff on both paths.
        let legacy = usage.estimated_cost_millicents();
        let registry = cost_for("claude-sonnet-5", &usage);
        // Legacy rounds per component: 30 + 2 + 4 + 150 = 186.
        assert_eq!(legacy, 186);
        // Registry totals in true millicents then converts: 185_250 → 185.
        assert_eq!(registry, 185);
        // Same scale, within per-component rounding slack.
        assert!(legacy.abs_diff(registry) <= 2, "legacy={legacy} registry={registry}");

        // Exact equality on a fixture where per-component rounding is clean.
        let clean = TokenUsage {
            input_tokens: 100_000,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            output_tokens: 100_000,
        };
        assert_eq!(cost_for("claude-sonnet-5", &clean), clean.estimated_cost_millicents());
        assert_eq!(cost_for("claude-sonnet-5", &clean), 180);
    }

    /// DeepSeek usage must bill at DeepSeek rates, not the old hardcoded
    /// Sonnet assumption (~30x cheaper at these rates).
    #[test]
    fn cost_for_deepseek_prices_at_deepseek_rates() {
        let usage = TokenUsage {
            input_tokens: 100_000,  // $0.23/MTok → 2_300 true mc
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            output_tokens: 100_000, // $0.34/MTok → 3_400 true mc
        };
        let deepseek = cost_for("deepseek-v3.2", &usage);
        assert_eq!(deepseek, 6); // 5_700 true mc → 6 legacy units
        // Qualified id resolves to the same entry.
        assert_eq!(cost_for("deepseek/deepseek-v3.2", &usage), deepseek);
        // Old behavior would have charged Sonnet rates (180).
        let old_assumption = usage.estimated_cost_millicents();
        assert_eq!(old_assumption, 180);
        assert!(deepseek * 20 < old_assumption);
    }

    /// Unknown model → byte-for-byte the legacy Sonnet fallback.
    #[test]
    fn cost_for_unknown_model_falls_back_to_legacy() {
        let usage = TokenUsage {
            input_tokens: 123_456,
            cache_read_tokens: 11_111,
            cache_creation_tokens: 22_222,
            output_tokens: 7_890,
        };
        assert_eq!(
            cost_for("totally-unknown-model", &usage),
            usage.estimated_cost_millicents()
        );
        // Above-cliff legacy doubling must survive the fallback too.
        let big = TokenUsage { input_tokens: 300_000, ..Default::default() };
        assert_eq!(
            cost_for("totally-unknown-model", &big),
            big.estimated_cost_millicents()
        );
    }

    /// Cliff threshold derives from the registry when the model is known.
    #[test]
    fn near_price_cliff_uses_registry_threshold() {
        let big = TokenUsage { input_tokens: 185_000, ..Default::default() };
        let small = TokenUsage { input_tokens: 170_000, ..Default::default() };
        // Anthropic 200K cliff → warn above 180K.
        assert!(near_price_cliff_for("claude-sonnet-5", &big));
        assert!(!near_price_cliff_for("claude-sonnet-5", &small));
        // Gemini Pro also carries a 200K cliff.
        assert!(near_price_cliff_for("gemini-3.1-pro", &big));
        // Registry model WITHOUT a cliff never warns.
        assert!(!near_price_cliff_for("deepseek-v3.2", &big));
        // Unknown model keeps the legacy fixed 180K margin.
        assert!(near_price_cliff_for("totally-unknown-model", &big));
        assert!(!near_price_cliff_for("totally-unknown-model", &small));
    }

    // ── #6.3: cost-pressure flag + evolution_event emission ─────────────────

    #[test]
    fn resolve_prediction_db_swaps_filename_in_same_dir() {
        let p = std::path::PathBuf::from("/Users/x/.duduclaw/cost_telemetry.db");
        assert_eq!(
            resolve_prediction_db_path(&p),
            std::path::PathBuf::from("/Users/x/.duduclaw/prediction.db"),
        );
    }

    #[test]
    fn resolve_prediction_db_handles_orphan_path() {
        // Defensive: if the telemetry path has no parent (raw filename),
        // we should still return a usable path rather than panicking.
        // `Path::parent()` returns `Some("")` for a bare filename, and
        // `.join("prediction.db")` on the empty path yields just
        // "prediction.db" — equivalent to "./prediction.db" at the OS
        // level. Either form opens the file in the current directory.
        let p = std::path::PathBuf::from("cost_telemetry.db");
        let resolved = resolve_prediction_db_path(&p);
        // Pin the actual behaviour rather than the aesthetic.
        assert_eq!(resolved, std::path::PathBuf::from("prediction.db"));
        // And confirm callers can use it without panic — that's the
        // real defensive guarantee.
        assert!(resolved.file_name().is_some());
    }

    #[tokio::test]
    async fn cliff_record_marks_cost_pressure_flag() {
        let dir = tempfile::tempdir().unwrap();
        let telemetry = CostTelemetry::new(&dir.path().join("cost.db")).unwrap();

        // Pre-condition: nobody under pressure.
        assert!(!telemetry.is_under_cost_pressure("eng-memory"));

        // Construct a usage that trips the cliff (>180K total_input).
        let usage = TokenUsage {
            input_tokens: 200_000,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            output_tokens: 100,
        };
        telemetry
            .record("eng-memory", RequestType::Chat, "claude-sonnet-4-6", &usage)
            .await;

        // The flag must now be set.
        assert!(
            telemetry.is_under_cost_pressure("eng-memory"),
            "is_under_cost_pressure must return true after a cliff trip"
        );
        // And only for the offending agent — not a global gate.
        assert!(!telemetry.is_under_cost_pressure("other-agent"));
    }

    #[tokio::test]
    async fn cost_pressure_flag_expires_after_ttl() {
        let dir = tempfile::tempdir().unwrap();
        let telemetry = CostTelemetry::new(&dir.path().join("cost.db")).unwrap();

        // Manually inject a stale timestamp so we don't have to wait
        // 1h in the test. Older than TTL → should be cleared on read.
        {
            let mut g = telemetry.cost_pressure_flags.lock().unwrap();
            g.insert(
                "stale-agent".to_string(),
                chrono::Utc::now()
                    - chrono::Duration::from_std(COST_PRESSURE_TTL).unwrap()
                    - chrono::Duration::seconds(60),
            );
            g.insert("fresh-agent".to_string(), chrono::Utc::now());
        }

        // Reading the flag prunes the stale entry.
        assert!(!telemetry.is_under_cost_pressure("stale-agent"));
        assert!(telemetry.is_under_cost_pressure("fresh-agent"));
    }

    #[tokio::test]
    async fn non_cliff_record_does_not_set_flag() {
        let dir = tempfile::tempdir().unwrap();
        let telemetry = CostTelemetry::new(&dir.path().join("cost.db")).unwrap();

        let usage = TokenUsage {
            input_tokens: 5_000,
            cache_read_tokens: 50_000,
            cache_creation_tokens: 0,
            output_tokens: 500,
        };
        telemetry
            .record("normal-agent", RequestType::Chat, "claude-sonnet-4-6", &usage)
            .await;

        // total_input = 55_000 → well below the 180K threshold. Flag
        // must stay clear.
        assert!(!telemetry.is_under_cost_pressure("normal-agent"));
    }
}
