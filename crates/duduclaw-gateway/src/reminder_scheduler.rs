//! Reminder scheduler — manages one-shot and agent-callback reminders.
//!
//! Uses a tokio time-wheel (`BTreeMap<DateTime, Vec<Reminder>>`) for
//! millisecond-precision wakeups.  Persists state to `reminders.jsonl`.
//! Two delivery modes:
//!   - **direct** — send a static message to a channel (zero LLM cost)
//!   - **agent_callback** — wake an agent, run a prompt, send the result
//!
//! Architecture: disk (`reminders.jsonl`) is the **single source of truth**.
//! The in-memory time wheel is a read-only cache rebuilt from disk on each
//! reload.  All mutations (status updates, GC) go through disk first,
//! serialized by a filesystem lockfile (`reminders.lock`).

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::claude_runner::call_claude_for_agent_with_type;
use duduclaw_agent::registry::AgentRegistry;
use duduclaw_security::input_guard;

// ── Constants ───────────────────────────────────────────────

/// Maximum concurrent reminder deliveries.
const MAX_CONCURRENT_DELIVER: usize = 4;

/// How often to reload from disk to pick up reminders added by MCP (separate process).
const RELOAD_INTERVAL_SECS: u64 = 10;

/// How often to run GC (remove delivered/cancelled/failed reminders older than 24h).
const GC_INTERVAL_SECS: u64 = 6 * 3600;

/// Maximum pending reminders per agent.
pub const MAX_REMINDERS_PER_AGENT: usize = 100;

/// Maximum message text length.
pub const MAX_MESSAGE_LEN: usize = 4000;

/// Maximum prompt length for agent_callback mode.
pub const MAX_PROMPT_LEN: usize = 2000;

/// Maximum days into the future a reminder can be set.
pub const MAX_FUTURE_DAYS: i64 = 365;

// ── Data structures ─────────────────────────────────────────

/// Delivery mode for a reminder.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReminderMode {
    Direct,
    AgentCallback,
}

/// Lifecycle status of a reminder.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReminderStatus {
    Pending,
    Delivered,
    Failed,
    Cancelled,
}

impl ReminderStatus {
    /// Match a status string (e.g. from MCP filter parameter).
    fn matches_str(&self, s: &str) -> bool {
        match self {
            Self::Pending => s == "pending",
            Self::Delivered => s == "delivered",
            Self::Failed => s == "failed",
            Self::Cancelled => s == "cancelled",
        }
    }
}

/// A single reminder entry persisted in `reminders.jsonl`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reminder {
    pub id: String,
    #[serde(default = "default_agent")]
    pub agent_id: String,
    pub trigger_at: DateTime<Utc>,
    pub channel: String,
    pub chat_id: String,
    /// Static message text (used when mode == Direct).
    #[serde(default)]
    pub message: Option<String>,
    /// Prompt to send to the agent (used when mode == AgentCallback).
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default = "default_mode")]
    pub mode: ReminderMode,
    #[serde(default = "default_pending")]
    pub status: ReminderStatus,
    #[serde(default)]
    pub created_at: Option<String>,
    /// Error message if delivery failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

fn default_agent() -> String {
    "default".to_string()
}
fn default_mode() -> ReminderMode {
    ReminderMode::Direct
}
fn default_pending() -> ReminderStatus {
    ReminderStatus::Pending
}

/// Result of a single reminder delivery attempt.
struct DeliveryResult {
    id: String,
    status: ReminderStatus,
    error: Option<String>,
}

// ── Time parsing ────────────────────────────────────────────

/// Parse a time specification into an absolute `DateTime<Utc>`.
///
/// Supports:
/// - ISO 8601: `2026-04-07T15:00:00+08:00`
/// - Relative: `30s`, `5m`, `2h`, `1d`
/// - Mixed relative: `1h30m`, `2h15m30s`
pub fn parse_time_spec(input: &str) -> Result<DateTime<Utc>, String> {
    let trimmed = input.trim();

    // Try ISO 8601 first
    if let Ok(dt) = DateTime::parse_from_rfc3339(trimmed) {
        return Ok(dt.with_timezone(&Utc));
    }
    // Try other common ISO formats
    if let Ok(dt) = chrono::DateTime::parse_from_str(trimmed, "%Y-%m-%dT%H:%M:%S%z") {
        return Ok(dt.with_timezone(&Utc));
    }

    // Parse relative duration: e.g. "5m", "2h", "1d", "1h30m", "2h15m30s"
    parse_relative_duration(trimmed).map(|dur| Utc::now() + dur)
}

/// Parse a relative duration string like `5m`, `2h30m`, `1d`.
/// Every number **must** have an explicit unit suffix (s/m/h/d).
fn parse_relative_duration(input: &str) -> Result<Duration, String> {
    if input.is_empty() {
        return Err("Empty time specification".to_string());
    }

    let mut total_secs: i64 = 0;
    let mut num_buf = String::new();
    let mut found_any = false;

    for ch in input.chars() {
        if ch.is_ascii_digit() {
            num_buf.push(ch);
        } else {
            if num_buf.is_empty() {
                return Err(format!("Unexpected character '{ch}' without preceding number"));
            }
            let n: i64 = num_buf
                .parse()
                .map_err(|_| format!("Invalid number in time spec: '{num_buf}'"))?;
            num_buf.clear();

            let secs = match ch {
                's' => n,
                'm' => n.checked_mul(60).ok_or("Time specification overflow")?,
                'h' => n.checked_mul(3600).ok_or("Time specification overflow")?,
                'd' => n.checked_mul(86400).ok_or("Time specification overflow")?,
                _ => return Err(format!("Unknown time unit: '{ch}'. Use s/m/h/d")),
            };
            total_secs = total_secs
                .checked_add(secs)
                .ok_or("Time specification overflow")?;
            found_any = true;
        }
    }

    // Reject trailing number without unit
    if !num_buf.is_empty() {
        return Err(format!(
            "Number '{num_buf}' missing unit suffix. Use s/m/h/d (e.g. '{num_buf}m')"
        ));
    }

    if !found_any || total_secs <= 0 {
        return Err(format!(
            "Duration must be positive. Got: '{input}'"
        ));
    }

    Ok(Duration::seconds(total_secs))
}

// ── Channel send (shared utility) ───────────────────────────

/// Send a text message to a channel.  Reads tokens from `config.toml`.
///
/// This is the shared implementation used by both the MCP `send_message`
/// handler and the `ReminderScheduler`.
pub async fn send_channel_message(
    home_dir: &Path,
    http: &reqwest::Client,
    channel: &str,
    chat_id: &str,
    text: &str,
) -> Result<(), String> {
    let config = read_config(home_dir)
        .await
        .ok_or_else(|| "Could not read config.toml".to_string())?;

    match channel {
        "telegram" => {
            let token = decrypt_channel_token(&config, "telegram_bot_token_enc", "telegram_bot_token", home_dir);
            if token.is_empty() {
                return Err("telegram_bot_token not configured".to_string());
            }
            let url = format!("https://api.telegram.org/bot{token}/sendMessage");
            let resp = http
                .post(&url)
                .json(&serde_json::json!({ "chat_id": chat_id, "text": text }))
                .send()
                .await
                .map_err(|_| "Telegram network error".to_string())?;
            if resp.status().is_success() {
                Ok(())
            } else {
                Err(format!("Telegram API returned {}", resp.status()))
            }
        }
        "line" => {
            let token = decrypt_channel_token(&config, "line_channel_token_enc", "line_channel_token", home_dir);
            if token.is_empty() {
                return Err("line_channel_token not configured".to_string());
            }
            let resp = http
                .post("https://api.line.me/v2/bot/message/push")
                .header("Authorization", format!("Bearer {token}"))
                .json(&serde_json::json!({
                    "to": chat_id,
                    "messages": [{"type": "text", "text": text}]
                }))
                .send()
                .await
                .map_err(|_| "LINE network error".to_string())?;
            if resp.status().is_success() {
                Ok(())
            } else {
                Err(format!("LINE API returned {}", resp.status()))
            }
        }
        "discord" => {
            // Discord channel IDs are numeric snowflakes
            if chat_id.is_empty() || !chat_id.chars().all(|c| c.is_ascii_digit()) {
                return Err(format!("Invalid Discord channel ID: '{chat_id}' (must be numeric)"));
            }
            let token = decrypt_channel_token(&config, "discord_bot_token_enc", "discord_bot_token", home_dir);
            if token.is_empty() {
                return Err("discord_bot_token not configured".to_string());
            }
            let url = format!("https://discord.com/api/v10/channels/{chat_id}/messages");
            let resp = http
                .post(&url)
                .header("Authorization", format!("Bot {token}"))
                .json(&serde_json::json!({ "content": text }))
                .send()
                .await
                .map_err(|_| "Discord network error".to_string())?;
            if resp.status().is_success() {
                Ok(())
            } else {
                Err(format!("Discord API returned {}", resp.status()))
            }
        }
        _ => Err(format!("Unknown channel: {channel}")),
    }
}

/// Validate a Discord channel ID (must be numeric snowflake).
pub fn is_valid_discord_chat_id(chat_id: &str) -> bool {
    !chat_id.is_empty() && chat_id.chars().all(|c| c.is_ascii_digit())
}

// ── Config helpers (duplicated from mcp.rs to avoid circular deps) ──

async fn read_config(home_dir: &Path) -> Option<toml::Table> {
    let path = home_dir.join("config.toml");
    let content = tokio::fs::read_to_string(&path).await.ok()?;
    content.parse().ok()
}

fn load_crypto_engine(home_dir: &Path) -> Option<duduclaw_security::crypto::CryptoEngine> {
    let keyfile = home_dir.join(".keyfile");
    let bytes = std::fs::read(&keyfile).ok()?;
    if bytes.len() == 32 {
        let mut key = [0u8; 32];
        key.copy_from_slice(&bytes);
        duduclaw_security::crypto::CryptoEngine::new(&key).ok()
    } else {
        None
    }
}

fn decrypt_encrypted_value(encrypted: &str, home_dir: &Path) -> Option<String> {
    if encrypted.is_empty() {
        return None;
    }
    let engine = load_crypto_engine(home_dir)?;
    let plain = engine.decrypt_string(encrypted).ok()?;
    if plain.is_empty() { None } else { Some(plain) }
}

fn decrypt_channel_token(config: &toml::Table, enc_key: &str, plain_key: &str, home_dir: &Path) -> String {
    let channels = config.get("channels").and_then(|c| c.as_table());
    if let Some(enc_val) = channels.and_then(|c| c.get(enc_key)).and_then(|v| v.as_str()) {
        if let Some(decrypted) = decrypt_encrypted_value(enc_val, home_dir) {
            return decrypted;
        }
    }
    channels
        .and_then(|c| c.get(plain_key))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

// ── JSONL persistence (lockfile-protected) ───────────────────

/// Lockfile path for serializing all writes to `reminders.jsonl`.
fn lock_path(home_dir: &Path) -> PathBuf {
    home_dir.join("reminders.lock")
}

/// Acquire an exclusive file lock on `reminders.lock`.
/// Returns the locked File handle; the lock is released on drop.
fn acquire_lock(home_dir: &Path) -> Result<std::fs::File, String> {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .open(lock_path(home_dir))
        .map_err(|e| format!("Failed to open lockfile: {e}"))?;
    duduclaw_core::platform::flock_exclusive(&file)
        .map_err(|e| format!("flock failed: {e}"))?;
    Ok(file)
}

/// Load all reminders from `reminders.jsonl` (sync, for use inside spawn_blocking).
fn load_reminders_sync(home_dir: &Path) -> Vec<Reminder> {
    let path = home_dir.join("reminders.jsonl");
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|line| serde_json::from_str::<Reminder>(line).ok())
        .collect()
}

/// Load reminders from disk (async wrapper).
async fn load_reminders(home_dir: &Path) -> Vec<Reminder> {
    let home = home_dir.to_path_buf();
    tokio::task::spawn_blocking(move || load_reminders_sync(&home))
        .await
        .unwrap_or_default()
}

/// Rewrite the full `reminders.jsonl` atomically under lockfile protection.
///
/// Uses a unique temp filename (UUID) to avoid collisions, then renames.
/// The caller must already hold the lockfile, or pass `lock_guard` = None
/// to acquire it internally.
fn save_reminders_sync(home_dir: &Path, reminders: &[Reminder]) -> Result<(), String> {
    let path = home_dir.join("reminders.jsonl");
    let tmp_name = format!("reminders.{}.tmp", uuid::Uuid::new_v4());
    let tmp_path = home_dir.join(&tmp_name);

    let mut content = String::new();
    for r in reminders {
        if let Ok(json) = serde_json::to_string(r) {
            content.push_str(&json);
            content.push('\n');
        }
    }

    std::fs::write(&tmp_path, &content)
        .map_err(|e| format!("Failed to write temp reminders: {e}"))?;
    std::fs::rename(&tmp_path, &path)
        .map_err(|e| {
            // Clean up tmp on rename failure
            let _ = std::fs::remove_file(&tmp_path);
            format!("Failed to rename temp reminders: {e}")
        })?;
    Ok(())
}

/// Locked read-modify-write: load reminders, apply a mutation, save back.
/// All disk mutations to `reminders.jsonl` MUST go through this function.
fn mutate_reminders_sync(
    home_dir: &Path,
    mutate: impl FnOnce(&mut Vec<Reminder>),
) -> Result<(), String> {
    let _lock = acquire_lock(home_dir)?;
    let mut reminders = load_reminders_sync(home_dir);
    mutate(&mut reminders);
    save_reminders_sync(home_dir, &reminders)
}

/// Async wrapper for `mutate_reminders_sync`.
async fn mutate_reminders(
    home_dir: &Path,
    mutate: impl FnOnce(&mut Vec<Reminder>) + Send + 'static,
) -> Result<(), String> {
    let home = home_dir.to_path_buf();
    tokio::task::spawn_blocking(move || mutate_reminders_sync(&home, mutate))
        .await
        .map_err(|e| format!("spawn_blocking: {e}"))?
}

/// Append a single reminder to `reminders.jsonl` under lockfile protection.
pub async fn append_reminder(home_dir: &Path, reminder: &Reminder) -> Result<(), String> {
    let r = reminder.clone();
    mutate_reminders(home_dir, move |reminders| {
        reminders.push(r);
    })
    .await
}

/// Count pending reminders for a given agent, atomically with append.
/// Returns the count **before** the append. If count >= limit, does NOT append.
pub async fn append_reminder_checked(
    home_dir: &Path,
    reminder: &Reminder,
    max_per_agent: usize,
) -> Result<AppendResult, String> {
    let r = reminder.clone();
    let agent = reminder.agent_id.clone();
    let home = home_dir.to_path_buf();

    tokio::task::spawn_blocking(move || {
        let _lock = acquire_lock(&home)?;
        let mut reminders = load_reminders_sync(&home);

        let pending_count = reminders
            .iter()
            .filter(|existing| existing.status == ReminderStatus::Pending && existing.agent_id == agent)
            .count();

        if pending_count >= max_per_agent {
            return Ok(AppendResult::LimitReached(pending_count));
        }

        reminders.push(r);
        save_reminders_sync(&home, &reminders)?;
        Ok(AppendResult::Ok)
    })
    .await
    .map_err(|e| format!("spawn_blocking: {e}"))?
}

/// Result of a checked append operation.
pub enum AppendResult {
    Ok,
    LimitReached(usize),
}

// ── ReminderScheduler ───────────────────────────────────────

pub struct ReminderScheduler {
    home_dir: PathBuf,
    registry: Arc<RwLock<AgentRegistry>>,
    /// Time-wheel: read-only cache rebuilt from disk on each reload.
    time_wheel: RwLock<BTreeMap<DateTime<Utc>, Vec<Reminder>>>,
    /// IDs of reminders that have been fired (prevents re-delivery if
    /// disk status update fails).  Cleared on each successful reload.
    fired_ids: RwLock<HashSet<String>>,
    /// Shared HTTP client with timeout (reused across all deliveries).
    http: reqwest::Client,
    semaphore: Arc<tokio::sync::Semaphore>,
}

impl ReminderScheduler {
    pub fn new(
        home_dir: PathBuf,
        registry: Arc<RwLock<AgentRegistry>>,
    ) -> Self {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("Failed to create HTTP client");

        Self {
            home_dir,
            registry,
            time_wheel: RwLock::new(BTreeMap::new()),
            fired_ids: RwLock::new(HashSet::new()),
            http,
            semaphore: Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_DELIVER)),
        }
    }

    /// Reload pending reminders from disk into the time wheel.
    async fn reload(&self) {
        let reminders = load_reminders(&self.home_dir).await;
        let fired = self.fired_ids.read().await;
        let mut wheel = self.time_wheel.write().await;
        wheel.clear();

        let mut pending_count = 0usize;
        for r in &reminders {
            if r.status == ReminderStatus::Pending && !fired.contains(&r.id) {
                wheel.entry(r.trigger_at).or_default().push(r.clone());
                pending_count += 1;
            }
        }

        if pending_count > 0 {
            info!(pending = pending_count, "Reminders reloaded from disk");
        }
    }

    /// Main scheduler loop.
    ///
    /// Disk is the single source of truth.  The loop:
    /// 1. Reload from disk (skipping fired_ids)
    /// 2. Fire due reminders → collect results
    /// 3. Batch-update statuses on disk (single locked write)
    /// 4. Clear fired_ids on success
    /// 5. Sleep until next trigger or reload interval
    pub async fn run(self: Arc<Self>) {
        self.reload().await;
        info!("Reminder scheduler running");

        // Spawn GC task (reads/writes disk directly under lockfile)
        let gc_home = self.home_dir.clone();
        tokio::spawn(async move {
            // Run first GC after 10 minutes to catch stale entries on startup
            tokio::time::sleep(std::time::Duration::from_secs(600)).await;
            loop {
                run_gc(&gc_home).await;
                tokio::time::sleep(std::time::Duration::from_secs(GC_INTERVAL_SECS)).await;
            }
        });

        // Main delivery loop
        loop {
            let next_wake = {
                let wheel = self.time_wheel.read().await;
                wheel.keys().next().copied()
            };

            let now = Utc::now();
            let reload_deadline = now + Duration::seconds(RELOAD_INTERVAL_SECS as i64);

            let sleep_until = match next_wake {
                Some(wake_at) if wake_at < reload_deadline => wake_at,
                _ => reload_deadline,
            };

            if sleep_until > now {
                let sleep_dur = (sleep_until - now)
                    .to_std()
                    .unwrap_or(std::time::Duration::from_millis(100));
                tokio::time::sleep(sleep_dur).await;
            }

            // Fire due reminders and batch-update disk
            self.fire_due_reminders().await;

            // Reload from disk to pick up new/cancelled reminders
            self.reload().await;
        }
    }

    /// Fire all reminders whose trigger_at <= now.
    ///
    /// 1. Deliver concurrently (up to MAX_CONCURRENT_DELIVER)
    /// 2. Collect all results
    /// 3. Apply all status updates in a single locked disk write
    async fn fire_due_reminders(&self) {
        let now = Utc::now();
        let mut to_fire = Vec::new();

        {
            let mut wheel = self.time_wheel.write().await;
            let due_keys: Vec<DateTime<Utc>> = wheel
                .range(..=now)
                .map(|(k, _)| *k)
                .collect();

            for key in due_keys {
                if let Some(reminders) = wheel.remove(&key) {
                    to_fire.extend(reminders);
                }
            }
        }

        if to_fire.is_empty() {
            return;
        }

        info!(count = to_fire.len(), "Firing due reminders");

        // Mark all as fired immediately (prevents re-delivery on reload)
        {
            let mut fired = self.fired_ids.write().await;
            for r in &to_fire {
                fired.insert(r.id.clone());
            }
        }

        // Deliver concurrently, collect results
        let mut handles = Vec::new();
        for reminder in to_fire {
            let home = self.home_dir.clone();
            let registry = self.registry.clone();
            let sem = self.semaphore.clone();
            let http = self.http.clone();

            handles.push(tokio::spawn(async move {
                let Ok(_permit) = sem.acquire().await else {
                    warn!(id = %reminder.id, "Semaphore closed, skipping reminder");
                    return DeliveryResult {
                        id: reminder.id.clone(),
                        status: ReminderStatus::Failed,
                        error: Some("Semaphore closed".to_string()),
                    };
                };

                let result = deliver_reminder(&home, &http, &registry, &reminder).await;

                match result {
                    Ok(()) => DeliveryResult {
                        id: reminder.id.clone(),
                        status: ReminderStatus::Delivered,
                        error: None,
                    },
                    Err(e) => {
                        warn!(id = %reminder.id, "Reminder delivery failed: {e}");
                        DeliveryResult {
                            id: reminder.id.clone(),
                            status: ReminderStatus::Failed,
                            error: Some(e),
                        }
                    }
                }
            }));
        }

        // Collect all results
        let mut results = Vec::new();
        for handle in handles {
            match handle.await {
                Ok(r) => results.push(r),
                Err(e) => warn!("Reminder delivery task panicked: {e}"),
            }
        }

        if results.is_empty() {
            return;
        }

        // Collect IDs of this batch before moving results into the closure
        let batch_ids: Vec<String> = results.iter().map(|r| r.id.clone()).collect();

        // Batch-update all statuses in a single locked disk write
        let home = self.home_dir.clone();
        let update_ok = mutate_reminders(&home, move |reminders| {
            for result in results {
                if let Some(r) = reminders.iter_mut().find(|r| r.id == result.id) {
                    r.status = result.status;
                    r.error = result.error;
                }
            }
        })
        .await;

        match update_ok {
            Ok(()) => {
                // Only remove THIS batch's IDs from fired_ids (not other batches'
                // IDs that may still be guarding against re-delivery after a
                // previous disk write failure).
                let mut fired = self.fired_ids.write().await;
                for id in &batch_ids {
                    fired.remove(id);
                }
            }
            Err(e) => {
                // Keep fired_ids so these reminders are NOT re-delivered on next reload
                warn!("Failed to batch-update reminder statuses: {e}");
            }
        }
    }
}

/// Garbage-collect old delivered/cancelled/failed reminders from disk.
async fn run_gc(home_dir: &Path) {
    let cutoff = Utc::now() - Duration::hours(24);
    let home = home_dir.to_path_buf();

    let result = tokio::task::spawn_blocking(move || {
        let _lock = acquire_lock(&home)?;
        let mut reminders = load_reminders_sync(&home);
        let before = reminders.len();

        reminders.retain(|r| {
            // Always keep pending reminders
            if r.status == ReminderStatus::Pending {
                return true;
            }
            // Remove delivered/cancelled/failed older than 24h
            // If created_at is missing, use trigger_at as fallback
            let ts = r.created_at
                .as_ref()
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                .map(|dt| dt.with_timezone(&Utc))
                .unwrap_or(r.trigger_at);
            ts > cutoff
        });

        let removed = before - reminders.len();
        if removed > 0 {
            save_reminders_sync(&home, &reminders)?;
            info!(removed, "Reminder GC completed");
        }
        Ok::<(), String>(())
    })
    .await;

    if let Err(e) = result {
        warn!("Reminder GC failed: {e:?}");
    }
}

/// Deliver a single reminder.
async fn deliver_reminder(
    home_dir: &Path,
    http: &reqwest::Client,
    registry: &Arc<RwLock<AgentRegistry>>,
    reminder: &Reminder,
) -> Result<(), String> {
    let text = match reminder.mode {
        ReminderMode::Direct => {
            reminder
                .message
                .clone()
                .unwrap_or_else(|| "(empty reminder)".to_string())
        }
        ReminderMode::AgentCallback => {
            let prompt = reminder
                .prompt
                .as_deref()
                .unwrap_or("(no prompt provided)");

            // Scan prompt for injection before sending to Claude
            let scan = input_guard::scan_input(prompt, input_guard::DEFAULT_BLOCK_THRESHOLD);
            if scan.blocked {
                return Err(format!(
                    "Prompt injection detected in reminder {}: {}",
                    reminder.id, scan.summary
                ));
            }

            let full_prompt = format!(
                "[Reminder Callback: {}] {}",
                reminder.id, prompt
            );

            call_claude_for_agent_with_type(
                home_dir,
                registry,
                &reminder.agent_id,
                &full_prompt,
                crate::cost_telemetry::RequestType::Cron,
            )
            .await
            .map_err(|e| format!("Agent callback failed: {e}"))?
        }
    };

    // Retry once on failure
    let result = send_channel_message(home_dir, http, &reminder.channel, &reminder.chat_id, &text).await;
    if let Err(first_err) = result {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        send_channel_message(home_dir, http, &reminder.channel, &reminder.chat_id, &text)
            .await
            .map_err(|e| format!("Delivery failed after retry (first: {first_err}): {e}"))?;
    }

    info!(
        id = %reminder.id,
        channel = %reminder.channel,
        mode = ?reminder.mode,
        "Reminder delivered"
    );
    Ok(())
}

// ── Public helpers for MCP tools ────────────────────────────

/// List reminders, optionally filtered by status and agent_id.
pub async fn list_reminders(
    home_dir: &Path,
    status_filter: Option<&str>,
    agent_filter: Option<&str>,
) -> Vec<Reminder> {
    let reminders = load_reminders(home_dir).await;
    reminders
        .into_iter()
        .filter(|r| {
            if let Some(s) = status_filter {
                if !r.status.matches_str(s) {
                    return false;
                }
            }
            if let Some(a) = agent_filter {
                if r.agent_id != a {
                    return false;
                }
            }
            true
        })
        .collect()
}

/// Cancel a reminder by ID.  Validates agent ownership.
/// Returns true if found and cancelled.
pub async fn cancel_reminder(
    home_dir: &Path,
    id: &str,
    caller_agent: Option<&str>,
) -> Result<bool, String> {
    let id_owned = id.to_string();
    let caller_owned = caller_agent.map(|s| s.to_string());
    let home = home_dir.to_path_buf();

    tokio::task::spawn_blocking(move || {
        let _lock = acquire_lock(&home)?;
        let mut reminders = load_reminders_sync(&home);
        let mut found = false;

        for r in &mut reminders {
            if r.id == id_owned && r.status == ReminderStatus::Pending {
                if let Some(ref caller) = caller_owned {
                    if r.agent_id != *caller {
                        return Err("Permission denied: you can only cancel your own reminders".to_string());
                    }
                }
                r.status = ReminderStatus::Cancelled;
                found = true;
                break;
            }
        }

        if found {
            save_reminders_sync(&home, &reminders)?;
        }
        Ok(found)
    })
    .await
    .map_err(|e| format!("spawn_blocking: {e}"))?
}

// ── Entry point ─────────────────────────────────────────────

/// Start the reminder scheduler as a background task.
pub fn start_reminder_scheduler(
    home_dir: PathBuf,
    registry: Arc<RwLock<AgentRegistry>>,
) -> tokio::task::JoinHandle<()> {
    let scheduler = Arc::new(ReminderScheduler::new(home_dir, registry));
    tokio::spawn(async move {
        scheduler.run().await;
    })
}
