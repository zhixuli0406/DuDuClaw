//! OS-native Phase 1 wiring: filesystem watchers → autopilot bus + stats file.
//!
//! At gateway startup [`init_os_watchers`] scans the agent registry and, for
//! every agent that has `[capabilities] os_native = true` **and** a non-empty
//! `[os_watch] paths` list, starts one `duduclaw_os::OsWatcher` into the shared
//! [`OsWatcherRegistry`]. Each watcher's debounced/rate-limited events are
//! forwarded onto the same `broadcast::Sender<AutopilotEvent>` the rest of the
//! engine consumes, as [`AutopilotEvent::OsFileEvent`].
//!
//! Because the MCP server runs out-of-process and cannot see in-process watcher
//! state, [`spawn_stats_writer`] persists per-agent counters to
//! `<home>/os_watch_stats.json` every 60s (under `with_file_lock`, project
//! convention #3); the `os_watch_status` MCP tool reads that file.
//!
//! **Config hot reload (v1.39):** the registry lives in `AppState`, so the
//! `agents.update` RPC can stop/start a single agent's watcher in place after an
//! `os_native` / `[os_watch]` edit — no gateway restart. Each entry's forwarder
//! task OWNS its [`WatchHandle`], so aborting the task drops the handle and stops
//! the OS watch; a cloned `Arc<WatchStats>` lets the stats writer read live
//! counters without touching the task.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use tokio::sync::broadcast;
use tokio::sync::{Mutex, RwLock};
use tokio::task::JoinHandle;
use tracing::{info, warn};

use duduclaw_agent::registry::AgentRegistry;
use duduclaw_os::watch::{OsWatcher, WatchConfig, WatchStats};

use crate::autopilot_engine::AutopilotEvent;

/// How often the supervisor persists watcher stats to disk.
const STATS_WRITE_INTERVAL: Duration = Duration::from_secs(60);
/// Public so the MCP tool and tests agree on the stats file name.
pub const STATS_FILE_NAME: &str = "os_watch_stats.json";

/// Read `[os_watch]` from an agent's `agent.toml` into a [`WatchConfig`].
///
/// Returns `None` when the file is missing/malformed, the `[os_watch]` table is
/// absent, or `paths` is empty (no path is ever watched by default). This is the
/// additive raw-TOML parse pattern used by `approval_required_tools` — it never
/// touches the serde `AgentConfig` struct, so an unrelated new key can't break
/// existing configs.
pub fn read_os_watch_config(agent_dir: &Path) -> Option<WatchConfig> {
    let path = agent_dir.join("agent.toml");
    let text = std::fs::read_to_string(&path).ok()?;
    let value: toml::Value = match toml::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            warn!(?path, error = %e, "malformed agent.toml — [os_watch] ignored");
            return None;
        }
    };
    let tbl = value.get("os_watch")?.as_table()?;

    let paths: Vec<PathBuf> = tbl
        .get("paths")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(duduclaw_core::expand_tilde)
                .collect()
        })
        .unwrap_or_default();
    if paths.is_empty() {
        return None;
    }

    let ignore: Vec<String> = tbl
        .get("ignore")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(String::from)
                .collect()
        })
        .unwrap_or_default();

    let debounce_ms = tbl
        .get("debounce_ms")
        .and_then(|v| v.as_integer())
        .map(|i| i.max(1) as u64)
        .unwrap_or(duduclaw_os::watch::DEFAULT_DEBOUNCE_MS);

    let max_events_per_min = tbl
        .get("max_events_per_min")
        .and_then(|v| v.as_integer())
        .map(|i| i.clamp(1, u32::MAX as i64) as u32)
        .unwrap_or(duduclaw_os::watch::DEFAULT_MAX_EVENTS_PER_MIN);

    Some(WatchConfig {
        paths,
        ignore,
        debounce_ms,
        max_events_per_min,
    })
}

/// Read the raw `[os_watch]` table from an agent's `agent.toml` as JSON for the
/// dashboard edit form (`agents.inspect`). Unlike [`read_os_watch_config`] this
/// does NOT expand `~` or apply defaults — it echoes exactly what's on disk so
/// the operator edits their own values. Returns `Null` when absent/malformed.
pub fn read_os_watch_json(agent_dir: &Path) -> serde_json::Value {
    let path = agent_dir.join("agent.toml");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return serde_json::Value::Null;
    };
    let Ok(value) = toml::from_str::<toml::Value>(&text) else {
        return serde_json::Value::Null;
    };
    match value.get("os_watch") {
        Some(tbl) => serde_json::to_value(tbl).unwrap_or(serde_json::Value::Null),
        None => serde_json::Value::Null,
    }
}

/// One running watcher, keyed by agent id inside the [`OsWatcherRegistry`].
///
/// `forwarder` OWNS the [`WatchHandle`] (moved into the task) — dropping the
/// task (via `abort` or channel close) stops the OS watch. `stats` is a cloned
/// `Arc<WatchStats>` so the stats writer reads live counters without racing the
/// task; `watched_paths` is snapshotted once at start (immutable for the life of
/// the watcher).
struct WatcherEntry {
    forwarder: JoinHandle<()>,
    stats: Arc<WatchStats>,
    watched_paths: Vec<String>,
}

/// Shared registry of running per-agent OS watchers. Held in `AppState` so the
/// `agents.update` RPC can hot stop/start one agent's watcher without a gateway
/// restart (the v1.39 hot-reload path). Construct once via [`OsWatcherRegistry::new`].
pub struct OsWatcherRegistry {
    watchers: Mutex<HashMap<String, WatcherEntry>>,
    home_dir: PathBuf,
}

impl OsWatcherRegistry {
    /// Create an empty registry bound to the DuDuClaw home dir (where the stats
    /// file is written).
    pub fn new(home_dir: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            watchers: Mutex::new(HashMap::new()),
            home_dir,
        })
    }

    /// Start (or restart) the watcher for one agent from its current
    /// `[os_watch]` config, forwarding events onto `tx`. Any existing watcher
    /// for the agent is stopped first (abort → drops its `WatchHandle`).
    ///
    /// Returns `true` iff a watcher is now running — i.e. the agent has a valid,
    /// non-empty `[os_watch] paths` and the OS watcher spawned. A missing table,
    /// empty paths, or a spawn error all return `false` (no entry registered).
    pub async fn start_agent(
        &self,
        agent_id: &str,
        agent_dir: &Path,
        tx: broadcast::Sender<AutopilotEvent>,
    ) -> bool {
        // Stop any prior watcher first so a config edit never leaves two
        // watchers on overlapping paths.
        self.stop_agent(agent_id).await;

        let Some(cfg) = read_os_watch_config(agent_dir) else {
            return false;
        };
        match OsWatcher::start(cfg) {
            Ok((mut rx, handle)) => {
                let stats = handle.stats().clone();
                let watched_paths = handle.watched_paths().to_vec();
                info!(
                    agent = %agent_id,
                    paths = ?watched_paths,
                    "OS filesystem watcher started"
                );
                let tx_cl = tx;
                let aid = agent_id.to_string();
                let forwarder = tokio::spawn(async move {
                    // Own the handle for the task's lifetime: dropping it (on
                    // abort or channel close) stops the OS watch.
                    let _watch = handle;
                    while let Some(ev) = rx.recv().await {
                        // A send error only means there are currently no
                        // autopilot subscribers — safe to ignore.
                        let _ = tx_cl.send(AutopilotEvent::OsFileEvent {
                            agent_id: aid.clone(),
                            path: ev.path,
                            change: ev.kind.as_str().to_string(),
                        });
                    }
                });
                self.watchers.lock().await.insert(
                    agent_id.to_string(),
                    WatcherEntry {
                        forwarder,
                        stats,
                        watched_paths,
                    },
                );
                true
            }
            Err(e) => {
                warn!(agent = %agent_id, error = %e, "failed to start OS watcher");
                false
            }
        }
    }

    /// Stop and deregister the watcher for one agent (abort → drops its
    /// `WatchHandle`, stopping the OS watch). Returns whether one was running.
    pub async fn stop_agent(&self, agent_id: &str) -> bool {
        if let Some(entry) = self.watchers.lock().await.remove(agent_id) {
            entry.forwarder.abort();
            true
        } else {
            false
        }
    }

    /// Number of running watchers (test / status helper).
    pub async fn len(&self) -> usize {
        self.watchers.lock().await.len()
    }

    /// True when no watcher is currently running.
    pub async fn is_empty(&self) -> bool {
        self.watchers.lock().await.is_empty()
    }

    /// Serialize the current counters and atomically write them under an
    /// advisory lock (convention #3: the MCP subprocess reads this file
    /// concurrently). To avoid creating the file when OS-native is never used,
    /// an empty registry only rewrites the file when it already exists (so a
    /// hot-removed last watcher is reflected as empty rather than left stale).
    async fn write_stats(&self) {
        let mut agents = HashMap::new();
        {
            let map = self.watchers.lock().await;
            for (id, e) in map.iter() {
                agents.insert(
                    id.clone(),
                    AgentWatchStats {
                        watched_paths: e.watched_paths.clone(),
                        emitted: e.stats.emitted.load(Ordering::Relaxed),
                        dropped: e.stats.dropped.load(Ordering::Relaxed),
                    },
                );
            }
        }
        let path = self.home_dir.join(STATS_FILE_NAME);
        if agents.is_empty() && !path.exists() {
            return;
        }
        let file = StatsFile {
            updated_at: chrono::Utc::now().to_rfc3339(),
            agents,
        };
        let json = match serde_json::to_string_pretty(&file) {
            Ok(j) => j,
            Err(e) => {
                warn!(error = %e, "failed to serialize os_watch stats");
                return;
            }
        };
        let res = tokio::task::spawn_blocking(move || {
            duduclaw_core::with_file_lock(&path, || std::fs::write(&path, json.as_bytes()))
        })
        .await;
        if let Ok(Err(e)) = res {
            warn!(error = %e, "failed to write os_watch_stats.json");
        }
    }
}

/// Per-agent stats serialized to `os_watch_stats.json`.
#[derive(serde::Serialize)]
struct AgentWatchStats {
    watched_paths: Vec<String>,
    emitted: u64,
    dropped: u64,
}

/// Root of the stats file.
#[derive(serde::Serialize)]
struct StatsFile {
    updated_at: String,
    agents: HashMap<String, AgentWatchStats>,
}

/// Scan the registry and start a watcher per eligible agent into `registry`.
///
/// `agent_id` keys use the agent's **directory name** (the same identifier the
/// MCP dispatch path and `os_watch_status` tool use), not the display name.
/// Idempotent per agent — [`OsWatcherRegistry::start_agent`] restarts in place.
pub async fn init_os_watchers(
    registry: Arc<OsWatcherRegistry>,
    agent_registry: Arc<RwLock<AgentRegistry>>,
    tx: broadcast::Sender<AutopilotEvent>,
) {
    // Collect (dir-name agent_id, agent_dir) for os_native agents.
    let candidates: Vec<(String, PathBuf)> = {
        let reg = agent_registry.read().await;
        reg.list()
            .iter()
            .filter(|a| a.config.capabilities.os_native)
            .filter_map(|a| {
                let id = a
                    .dir
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(String::from)?;
                Some((id, a.dir.clone()))
            })
            .collect()
    };

    if candidates.is_empty() {
        info!("no os_native agents configured — OS watchers not started");
        return;
    }

    for (agent_id, agent_dir) in candidates {
        if !registry
            .start_agent(&agent_id, &agent_dir, tx.clone())
            .await
        {
            info!(
                agent = %agent_id,
                "os_native enabled but no [os_watch] paths — watcher skipped"
            );
        }
    }
}

/// Spawn the periodic stats-writer loop for `registry`. Returned handle is held
/// by the gateway for the process lifetime (bg_handles). Persists per-agent
/// counters to `os_watch_stats.json` every [`STATS_WRITE_INTERVAL`].
pub fn spawn_stats_writer(registry: Arc<OsWatcherRegistry>) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            registry.write_stats().await;
            tokio::time::sleep(STATS_WRITE_INTERVAL).await;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_os_watch_config_expands_tilde_paths() {
        // `[os_watch] paths` entries with a leading `~` are expanded against the
        // user home via the shared `duduclaw_core::expand_tilde` helper (the
        // watcher then canonicalizes, which does NOT itself expand `~`).
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("agent.toml"),
            "[os_watch]\npaths = [\"~/Downloads\", \"/abs/inbox\"]\n",
        )
        .unwrap();
        let home = PathBuf::from(duduclaw_core::home_dir());
        let cfg = read_os_watch_config(dir.path()).expect("should parse");
        assert_eq!(cfg.paths[0], home.join("Downloads"));
        assert_eq!(cfg.paths[1], PathBuf::from("/abs/inbox"));
    }

    #[test]
    fn read_os_watch_config_absent_or_empty_is_none() {
        let dir = tempfile::tempdir().unwrap();
        // No agent.toml at all.
        assert!(read_os_watch_config(dir.path()).is_none());

        // agent.toml without [os_watch].
        std::fs::write(
            dir.path().join("agent.toml"),
            "[capabilities]\nos_native = true\n",
        )
        .unwrap();
        assert!(read_os_watch_config(dir.path()).is_none());

        // [os_watch] present but paths empty → None (never watch by default).
        std::fs::write(dir.path().join("agent.toml"), "[os_watch]\npaths = []\n").unwrap();
        assert!(read_os_watch_config(dir.path()).is_none());
    }

    #[test]
    fn read_os_watch_config_parses_fields() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("agent.toml"),
            r#"
[capabilities]
os_native = true

[os_watch]
paths = ["/abs/inbox", "~/Downloads"]
ignore = ["*.part"]
debounce_ms = 500
max_events_per_min = 12
"#,
        )
        .unwrap();
        let home = PathBuf::from(duduclaw_core::home_dir());
        let cfg = read_os_watch_config(dir.path()).expect("should parse");
        assert_eq!(cfg.paths.len(), 2);
        assert_eq!(cfg.paths[0], PathBuf::from("/abs/inbox"));
        assert_eq!(cfg.paths[1], home.join("Downloads"));
        assert_eq!(cfg.ignore, vec!["*.part".to_string()]);
        assert_eq!(cfg.debounce_ms, 500);
        assert_eq!(cfg.max_events_per_min, 12);
    }

    #[test]
    fn read_os_watch_config_defaults_debounce_and_cap() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("agent.toml"),
            "[os_watch]\npaths = [\"/abs/inbox\"]\n",
        )
        .unwrap();
        let cfg = read_os_watch_config(dir.path()).expect("should parse");
        assert_eq!(cfg.debounce_ms, duduclaw_os::watch::DEFAULT_DEBOUNCE_MS);
        assert_eq!(
            cfg.max_events_per_min,
            duduclaw_os::watch::DEFAULT_MAX_EVENTS_PER_MIN
        );
    }

    #[tokio::test]
    async fn registry_start_restart_stop_agent() {
        // Agent dir carries the [os_watch] config; a separate tempdir is the
        // actual watched path (must exist so the OS watcher can canonicalize it).
        let agent_dir = tempfile::tempdir().unwrap();
        let watched = tempfile::tempdir().unwrap();
        let write_cfg = |paths: &str| {
            std::fs::write(
                agent_dir.path().join("agent.toml"),
                format!("[os_watch]\npaths = [{paths}]\n"),
            )
            .unwrap();
        };
        write_cfg(&format!("\"{}\"", watched.path().display()));

        let home = tempfile::tempdir().unwrap();
        let reg = OsWatcherRegistry::new(home.path().to_path_buf());
        let (tx, _rx) = tokio::sync::broadcast::channel::<AutopilotEvent>(16);

        // Start → one watcher registered.
        assert!(reg.start_agent("a1", agent_dir.path(), tx.clone()).await);
        assert_eq!(reg.len().await, 1);

        // Restart in place (config edit) → still exactly one entry.
        assert!(reg.start_agent("a1", agent_dir.path(), tx.clone()).await);
        assert_eq!(reg.len().await, 1);

        // Stop → deregistered.
        assert!(reg.stop_agent("a1").await);
        assert_eq!(reg.len().await, 0);
        // Second stop is a no-op.
        assert!(!reg.stop_agent("a1").await);

        // Empty [os_watch] paths → not started, no entry, prior watcher cleared.
        assert!(reg.start_agent("a1", agent_dir.path(), tx.clone()).await);
        assert_eq!(reg.len().await, 1);
        write_cfg("");
        assert!(!reg.start_agent("a1", agent_dir.path(), tx).await);
        assert_eq!(reg.len().await, 0);
    }
}
