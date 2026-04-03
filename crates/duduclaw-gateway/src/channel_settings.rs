//! Per-channel, per-scope settings stored in SQLite with in-memory cache.
//!
//! Supports hierarchical settings: global → channel-type → scope (guild/chat).
//! Used for mention-only mode, channel whitelists, auto-thread, agent overrides, etc.
//!
//! Read-heavy operations use an in-memory HashMap cache to avoid Mutex contention
//! on the SQLite connection. Cache is invalidated on write (set/delete).

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use rusqlite::{params, Connection};
use tokio::sync::{Mutex, RwLock};
use tracing::info;

// ── Types ──────────────────────────────────────────────────────

/// Known setting keys (type-safe access).
pub mod keys {
    /// Whether the bot only responds when mentioned. Values: "true" / "false"
    pub const MENTION_ONLY: &str = "mention_only";
    /// JSON array of allowed channel/chat IDs. Empty array or missing = all allowed.
    pub const ALLOWED_CHANNELS: &str = "allowed_channels";
    /// Whether to auto-create threads for replies. Values: "true" / "false"
    pub const AUTO_THREAD: &str = "auto_thread";
    /// Override agent name for this scope.
    pub const AGENT_OVERRIDE: &str = "agent_override";
    /// Response mode: "embed" | "plain" | "auto"
    pub const RESPONSE_MODE: &str = "response_mode";
    /// Thread auto-archive duration in minutes: "60" | "1440" | "4320" | "10080"
    pub const THREAD_ARCHIVE_MINUTES: &str = "thread_archive_minutes";
}

/// Cache key: (channel_type, scope_id, key)
type CacheKey = (String, String, String);

/// Channel settings manager backed by SQLite with an in-memory read cache.
pub struct ChannelSettingsManager {
    conn: Mutex<Connection>,
    /// In-memory cache: read-heavy path avoids Mutex contention on SQLite connection.
    /// Populated on first read, invalidated on write.
    cache: Arc<RwLock<HashMap<CacheKey, Option<String>>>>,
}

impl ChannelSettingsManager {
    /// Open or create the channel settings database.
    pub fn new(db_path: &Path) -> Result<Self, String> {
        let conn = Connection::open(db_path).map_err(|e| e.to_string())?;
        if db_path.to_str() != Some(":memory:") {
            conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000;")
                .map_err(|e| e.to_string())?;
        }
        Self::init_tables(&conn)?;
        info!(?db_path, "Channel settings manager initialized");
        Ok(Self {
            conn: Mutex::new(conn),
            cache: Arc::new(RwLock::new(HashMap::new())),
        })
    }

    /// Initialize using an existing session database connection path.
    pub fn from_session_db(db_path: &Path) -> Result<Self, String> {
        let conn = Connection::open(db_path).map_err(|e| e.to_string())?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000;")
            .map_err(|e| e.to_string())?;
        Self::init_tables(&conn)?;
        info!(?db_path, "Channel settings (co-located with session DB)");
        Ok(Self {
            conn: Mutex::new(conn),
            cache: Arc::new(RwLock::new(HashMap::new())),
        })
    }

    fn init_tables(conn: &Connection) -> Result<(), String> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS channel_settings (
                channel_type TEXT NOT NULL,
                scope_id TEXT NOT NULL,
                key TEXT NOT NULL,
                value TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                PRIMARY KEY (channel_type, scope_id, key)
            );

            CREATE INDEX IF NOT EXISTS idx_channel_settings_scope
                ON channel_settings(channel_type, scope_id);"
        ).map_err(|e| e.to_string())?;
        Ok(())
    }

    fn cache_key(channel_type: &str, scope_id: &str, key: &str) -> CacheKey {
        (channel_type.to_string(), scope_id.to_string(), key.to_string())
    }

    /// Get a setting value. Returns `None` if not set.
    /// Uses in-memory cache for read-heavy path.
    pub async fn get(&self, channel_type: &str, scope_id: &str, key: &str) -> Option<String> {
        let ck = Self::cache_key(channel_type, scope_id, key);

        // Fast path: check cache (RwLock read — no contention with other readers)
        {
            let cache = self.cache.read().await;
            if let Some(cached) = cache.get(&ck) {
                return cached.clone();
            }
        }

        // Slow path: query DB and populate cache
        let conn = self.conn.lock().await;
        let result: Option<String> = conn.query_row(
            "SELECT value FROM channel_settings WHERE channel_type = ?1 AND scope_id = ?2 AND key = ?3",
            params![channel_type, scope_id, key],
            |row| row.get(0),
        ).ok();
        drop(conn);

        // Store in cache (including None to avoid repeated DB misses)
        let mut cache = self.cache.write().await;
        cache.insert(ck, result.clone());

        result
    }

    /// Get a setting with fallback: scope → global → default.
    pub async fn get_with_fallback(
        &self,
        channel_type: &str,
        scope_id: &str,
        key: &str,
        default: &str,
    ) -> String {
        if let Some(v) = self.get(channel_type, scope_id, key).await {
            return v;
        }
        if scope_id != "global" {
            if let Some(v) = self.get(channel_type, "global", key).await {
                return v;
            }
        }
        default.to_string()
    }

    /// Get a boolean setting with fallback.
    pub async fn get_bool(&self, channel_type: &str, scope_id: &str, key: &str, default: bool) -> bool {
        let val = self.get_with_fallback(channel_type, scope_id, key, if default { "true" } else { "false" }).await;
        val == "true"
    }

    /// Get allowed channels list (JSON array of strings).
    pub async fn get_allowed_channels(&self, channel_type: &str, scope_id: &str) -> Vec<String> {
        let val = self.get(channel_type, scope_id, keys::ALLOWED_CHANNELS).await
            .unwrap_or_default();
        if val.is_empty() {
            return Vec::new();
        }
        serde_json::from_str(&val).unwrap_or_else(|e| {
            tracing::warn!(key = "allowed_channels", error = %e, "Corrupt JSON in channel settings — falling back to allow-all");
            Vec::new()
        })
    }

    /// Set a setting value (upsert). Invalidates cache for this key.
    pub async fn set(&self, channel_type: &str, scope_id: &str, key: &str, value: &str) -> Result<(), String> {
        let conn = self.conn.lock().await;
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO channel_settings (channel_type, scope_id, key, value, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(channel_type, scope_id, key) DO UPDATE SET value = ?4, updated_at = ?5",
            params![channel_type, scope_id, key, value, now],
        ).map_err(|e| e.to_string())?;
        drop(conn);

        // Invalidate cache
        let mut cache = self.cache.write().await;
        let ck = Self::cache_key(channel_type, scope_id, key);
        cache.insert(ck, Some(value.to_string()));

        Ok(())
    }

    /// Delete a setting. Invalidates cache for this key.
    pub async fn delete(&self, channel_type: &str, scope_id: &str, key: &str) -> Result<(), String> {
        let conn = self.conn.lock().await;
        conn.execute(
            "DELETE FROM channel_settings WHERE channel_type = ?1 AND scope_id = ?2 AND key = ?3",
            params![channel_type, scope_id, key],
        ).map_err(|e| e.to_string())?;
        drop(conn);

        // Invalidate cache
        let mut cache = self.cache.write().await;
        let ck = Self::cache_key(channel_type, scope_id, key);
        cache.insert(ck, None);

        Ok(())
    }

    /// Get all settings for a scope.
    pub async fn get_all(&self, channel_type: &str, scope_id: &str) -> Vec<(String, String)> {
        let conn = self.conn.lock().await;
        let mut stmt = match conn.prepare(
            "SELECT key, value FROM channel_settings WHERE channel_type = ?1 AND scope_id = ?2"
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        stmt.query_map(params![channel_type, scope_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map(|rows| rows.filter_map(|r| r.ok()).collect())
        .unwrap_or_default()
    }

    /// Check if a channel_id is allowed for a given scope.
    /// Returns true if no whitelist is set (empty = allow all).
    pub async fn is_channel_allowed(&self, channel_type: &str, scope_id: &str, channel_id: &str) -> bool {
        let allowed = self.get_allowed_channels(channel_type, scope_id).await;
        if allowed.is_empty() {
            return true;
        }
        allowed.iter().any(|id| id == channel_id)
    }
}

// ── Tests ──────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    fn temp_db() -> (NamedTempFile, ChannelSettingsManager) {
        let tmp = NamedTempFile::new().unwrap();
        let mgr = ChannelSettingsManager::new(tmp.path()).unwrap();
        (tmp, mgr)
    }

    #[tokio::test]
    async fn test_set_and_get() {
        let (_tmp, mgr) = temp_db();
        mgr.set("discord", "guild123", "mention_only", "true").await.unwrap();
        assert_eq!(mgr.get("discord", "guild123", "mention_only").await, Some("true".to_string()));
    }

    #[tokio::test]
    async fn test_cache_hit() {
        let (_tmp, mgr) = temp_db();
        mgr.set("discord", "guild123", "mention_only", "true").await.unwrap();
        // First read populates cache
        let _ = mgr.get("discord", "guild123", "mention_only").await;
        // Second read should hit cache (no way to assert directly, but ensures no panic)
        assert_eq!(mgr.get("discord", "guild123", "mention_only").await, Some("true".to_string()));
    }

    #[tokio::test]
    async fn test_cache_invalidation_on_set() {
        let (_tmp, mgr) = temp_db();
        mgr.set("discord", "g1", "mention_only", "true").await.unwrap();
        assert_eq!(mgr.get("discord", "g1", "mention_only").await, Some("true".to_string()));
        // Update should invalidate cache
        mgr.set("discord", "g1", "mention_only", "false").await.unwrap();
        assert_eq!(mgr.get("discord", "g1", "mention_only").await, Some("false".to_string()));
    }

    #[tokio::test]
    async fn test_cache_invalidation_on_delete() {
        let (_tmp, mgr) = temp_db();
        mgr.set("discord", "g1", "mention_only", "true").await.unwrap();
        let _ = mgr.get("discord", "g1", "mention_only").await; // populate cache
        mgr.delete("discord", "g1", "mention_only").await.unwrap();
        assert_eq!(mgr.get("discord", "g1", "mention_only").await, None);
    }

    #[tokio::test]
    async fn test_fallback_to_global() {
        let (_tmp, mgr) = temp_db();
        mgr.set("discord", "global", "mention_only", "true").await.unwrap();
        let val = mgr.get_with_fallback("discord", "guild999", "mention_only", "false").await;
        assert_eq!(val, "true");
    }

    #[tokio::test]
    async fn test_scope_overrides_global() {
        let (_tmp, mgr) = temp_db();
        mgr.set("discord", "global", "mention_only", "true").await.unwrap();
        mgr.set("discord", "guild123", "mention_only", "false").await.unwrap();
        let val = mgr.get_with_fallback("discord", "guild123", "mention_only", "true").await;
        assert_eq!(val, "false");
    }

    #[tokio::test]
    async fn test_allowed_channels_empty() {
        let (_tmp, mgr) = temp_db();
        assert!(mgr.is_channel_allowed("discord", "guild123", "ch456").await);
    }

    #[tokio::test]
    async fn test_allowed_channels_whitelist() {
        let (_tmp, mgr) = temp_db();
        mgr.set("discord", "guild123", "allowed_channels", r#"["ch1","ch2"]"#).await.unwrap();
        assert!(mgr.is_channel_allowed("discord", "guild123", "ch1").await);
        assert!(!mgr.is_channel_allowed("discord", "guild123", "ch999").await);
    }

    #[tokio::test]
    async fn test_get_bool() {
        let (_tmp, mgr) = temp_db();
        mgr.set("telegram", "global", "mention_only", "true").await.unwrap();
        assert!(mgr.get_bool("telegram", "global", "mention_only", false).await);
        assert!(!mgr.get_bool("telegram", "global", "auto_thread", false).await);
    }

    #[tokio::test]
    async fn test_delete() {
        let (_tmp, mgr) = temp_db();
        mgr.set("slack", "global", "mention_only", "true").await.unwrap();
        mgr.delete("slack", "global", "mention_only").await.unwrap();
        assert_eq!(mgr.get("slack", "global", "mention_only").await, None);
    }

    #[tokio::test]
    async fn test_get_all() {
        let (_tmp, mgr) = temp_db();
        mgr.set("discord", "guild1", "mention_only", "true").await.unwrap();
        mgr.set("discord", "guild1", "auto_thread", "false").await.unwrap();
        let all = mgr.get_all("discord", "guild1").await;
        assert_eq!(all.len(), 2);
    }
}
