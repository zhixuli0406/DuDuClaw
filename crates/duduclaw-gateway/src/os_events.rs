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

/// `[os_watch] goal_template` / `goal_acceptance` (P3-4): kick off an
/// autonomous goal loop from an `os_file` event, instead of (or alongside) an
/// ordinary autopilot rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoalTemplateConfig {
    /// Goal description template. May reference `{path}`, `{file_name}`,
    /// `{kind}` placeholders — the exact field names
    /// `AutopilotEvent::OsFileEvent::to_fields()` exposes to rule authors, so
    /// the same mental model applies. Rendered against the **sanitized**
    /// (perception-neutralized) copy of those fields, never the raw text.
    pub template: String,
    /// Optional acceptance-criteria template (same placeholders). `None` ⇒
    /// the rendered goal description doubles as the acceptance basis, mirroring
    /// the `/goal` chat command's own default when no `||` clause is given.
    pub acceptance: Option<String>,
}

/// Read `[os_watch] goal_template` / `goal_acceptance` from an agent's
/// `agent.toml`.
///
/// Additive raw-TOML parse — same convention as [`read_os_watch_config`] /
/// `os_frontmost::read_frontmost_poll_secs` — never touches the serde
/// `AgentConfig` struct, so this key can't break existing configs. Returns
/// `None` (never kick off a goal) when the file/table/key is absent,
/// malformed, or `goal_template` is empty/whitespace-only — mirroring
/// `read_os_watch_config`'s "empty ⇒ never watch by default" rule.
pub fn read_goal_template_config(agent_dir: &Path) -> Option<GoalTemplateConfig> {
    let path = agent_dir.join("agent.toml");
    let text = std::fs::read_to_string(&path).ok()?;
    let value: toml::Value = match toml::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            warn!(?path, error = %e, "malformed agent.toml — [os_watch] goal_template ignored");
            return None;
        }
    };
    let tbl = value.get("os_watch")?.as_table()?;

    let template = tbl
        .get("goal_template")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())?
        .to_string();

    let acceptance = tbl
        .get("goal_acceptance")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from);

    Some(GoalTemplateConfig {
        template,
        acceptance,
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

    /// Live per-agent watcher snapshot for the `os.status` dashboard RPC
    /// (in-process — distinct from the `os_watch_stats.json` file the
    /// out-of-process MCP `os_watch_status` tool reads; both surfaces coexist,
    /// P2 rule #5). Keyed by dir-name agent id.
    pub async fn snapshot(&self) -> HashMap<String, AgentWatchSnapshot> {
        let map = self.watchers.lock().await;
        map.iter()
            .map(|(id, e)| {
                (
                    id.clone(),
                    AgentWatchSnapshot {
                        watched_paths: e.watched_paths.clone(),
                        emitted: e.stats.emitted.load(Ordering::Relaxed),
                        dropped: e.stats.dropped.load(Ordering::Relaxed),
                    },
                )
            })
            .collect()
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

/// Live per-agent watcher snapshot returned by [`OsWatcherRegistry::snapshot`]
/// for the in-process `os.status` dashboard RPC.
#[derive(Debug, Clone)]
pub struct AgentWatchSnapshot {
    pub watched_paths: Vec<String>,
    pub emitted: u64,
    pub dropped: u64,
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
/// `allowed` is the quota-resolved set of dir-name ids permitted to run
/// OS-native features (see [`resolve_os_native_allowed`]); an agent outside it
/// is skipped even when it declares `[os_watch] paths`.
pub async fn init_os_watchers(
    registry: Arc<OsWatcherRegistry>,
    agent_registry: Arc<RwLock<AgentRegistry>>,
    tx: broadcast::Sender<AutopilotEvent>,
    allowed: &std::collections::HashSet<String>,
) {
    // Collect (dir-name agent_id, agent_dir) for os_native agents within quota.
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
                if !allowed.contains(&id) {
                    return None;
                }
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

/// Outcome of applying the per-edition OS-native quota across the agent fleet
/// at gateway startup (fail-closed consistency with the write-time gate — both
/// consult `license_runtime::os_native_agent_quota`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OsNativeQuotaResolution {
    /// Dir-name ids permitted to run OS-native features this boot.
    pub allowed: std::collections::HashSet<String>,
    /// Over-quota dir-name ids, in the stable sort order that decided them
    /// (kept so the caller can warn/audit deterministically).
    pub skipped: Vec<String>,
}

/// Pure quota partition: given the stable-sorted os_native dir-name ids and an
/// optional quota, return which are allowed vs skipped. `None` quota ⇒ all
/// allowed. `Some(n)` ⇒ the first `n` (in the given order) are allowed, the
/// rest skipped. Deterministic: the caller sorts before calling so the same
/// agent always wins the single Personal seat across restarts.
pub fn partition_by_quota(sorted_ids: Vec<String>, quota: Option<u32>) -> OsNativeQuotaResolution {
    match quota {
        None => OsNativeQuotaResolution {
            allowed: sorted_ids.into_iter().collect(),
            skipped: Vec::new(),
        },
        Some(n) => {
            let n = n as usize;
            let allowed: std::collections::HashSet<String> =
                sorted_ids.iter().take(n).cloned().collect();
            let skipped: Vec<String> = sorted_ids.into_iter().skip(n).collect();
            OsNativeQuotaResolution { allowed, skipped }
        }
    }
}

/// Resolve which os_native agents may run OS-native features under `quota`.
///
/// Scans the registry for `[capabilities] os_native = true` agents (by dir
/// name), sorts them deterministically, and applies [`partition_by_quota`].
/// Over-quota agents are `warn!`-logged here; the caller (which holds the home
/// dir) writes the audit event — this keeps the resolver I/O-free and unit
/// testable. This is the SINGLE startup gate the three OS-native init paths
/// (`init_os_watchers` / `os_frontmost::init_frontmost_polling` /
/// `footprint_distill::init_footprint_distill`) share, so all three agree on
/// exactly which agents are live (fail-closed consistency).
pub async fn resolve_os_native_allowed(
    agent_registry: &RwLock<AgentRegistry>,
    quota: Option<u32>,
) -> OsNativeQuotaResolution {
    let mut ids: Vec<String> = {
        let reg = agent_registry.read().await;
        reg.list()
            .iter()
            .filter(|a| a.config.capabilities.os_native)
            .filter_map(|a| a.dir.file_name().and_then(|n| n.to_str()).map(String::from))
            .collect()
    };
    ids.sort();
    ids.dedup();

    let resolution = partition_by_quota(ids, quota);
    if !resolution.skipped.is_empty() {
        warn!(
            quota = ?quota,
            allowed = ?resolution.allowed,
            skipped = ?resolution.skipped,
            "OS-native quota exceeded — only the stable-first agents run OS-native features; \
             the rest are skipped at startup (upgrade to raise the quota)"
        );
    }
    resolution
}

/// True when at least one agent has `[capabilities] os_native = true`.
///
/// Used by the server startup path to explain a silent no-op: [`init_os_watchers`]
/// is only reachable when the autopilot bus (task store + autopilot store) is
/// initialized, so a "lean" gateway configuration without a task board would
/// otherwise never start OS watchers with no indication why. Cheap read-lock
/// scan — called once at startup, not on a hot path.
pub async fn any_os_native_agents(agent_registry: &RwLock<AgentRegistry>) -> bool {
    agent_registry
        .read()
        .await
        .list()
        .iter()
        .any(|a| a.config.capabilities.os_native)
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

/// P4-1 integration-gap closer: subscribe to the SAME autopilot broadcast the
/// AutopilotEngine consumes and persist `os_file` / `os_frontmost` events into
/// `events.db`, stamped `source = SOURCE_INTERNAL_BROADCAST`.
///
/// ## Why a subscriber bridge, not a direct write in the two forwarders
///
/// [`OsWatcherRegistry::start_agent`]'s forwarder task and
/// `os_frontmost::spawn_agent_poll` each already do exactly one thing —
/// translate a raw OS observation into an `AutopilotEvent` and `tx.send()` it.
/// Threading an `Arc<EventBusStore>` (plus the async write, plus error
/// handling) into both would touch two hot loops already covered by existing
/// tests, for a concern — durable perception history for
/// `rule_induction::RuleInductor`'s pattern detector — that is orthogonal to
/// "forward this event now". A single subscriber task is the smallest-diff
/// wiring, and it is not a new pattern in this module family:
/// `crate::interruptibility::InterruptibilityTracker::spawn` already
/// subscribes to this same broadcast for these same two event types.
///
/// ## Why the `source` marker
///
/// `crate::autopilot_engine::spawn_events_db_poll` tails `events.db` and
/// re-`tx.send()`s every row it finds onto the SAME broadcast channel — that
/// is how an MCP-subprocess-originated event (no other path sees it) reaches
/// the engine. Without a marker, persisting an ALREADY-broadcast os_file /
/// os_frontmost event here would let the poll pick the row back up and
/// broadcast it a second time: every autopilot rule match, every
/// `InterruptibilityTracker` count, every `ProactiveGate` decision for that
/// one OS observation would fire twice. Rows written by this bridge carry
/// `source = Some(SOURCE_INTERNAL_BROADCAST)`; `spawn_events_db_poll` checks
/// that marker and skips re-broadcasting those specific rows (see its own
/// doc), while `EventBusStore::fetch_since` — the API `RuleInductor` reads
/// directly, never through the broadcast bus — returns them unfiltered.
pub fn spawn_os_event_persistence(
    events: Arc<crate::events_store::EventBusStore>,
    mut rx: broadcast::Receiver<AutopilotEvent>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(ev @ AutopilotEvent::OsFileEvent { .. })
                | Ok(ev @ AutopilotEvent::OsFrontmostEvent { .. }) => {
                    let event_name = ev.event_name();
                    let payload = serde_json::Value::Object(ev.to_fields()).to_string();
                    let ts = chrono::Utc::now().to_rfc3339();
                    if let Err(e) = events
                        .append_with_source(
                            event_name,
                            &payload,
                            &ts,
                            Some(crate::events_store::SOURCE_INTERNAL_BROADCAST),
                        )
                        .await
                    {
                        warn!(error = %e, event = %event_name, "failed to persist os event to events.db");
                    }
                }
                // Not a perception event this bridge persists (e.g. TaskCreated,
                // AgentIdle, CepTrigger) — nothing to do.
                Ok(_) => {}
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    warn!(dropped = n, "os_event_persistence: broadcast lagged");
                }
                Err(broadcast::error::RecvError::Closed) => {
                    warn!("os_event_persistence: broadcast closed — persistence bridge stopping");
                    break;
                }
            }
        }
    })
}

// ── P4-3+: live dashboard event tail ─────────────────────────────────────

/// Per-connection forwarding cap for the `os.events.subscribe` live tail
/// (`server.rs`'s WS loop). A misbehaving/misconfigured watcher (e.g. a
/// `[os_watch]` path pointed at a directory under heavy write load) must
/// never be able to flood a single dashboard socket — events beyond this
/// many per rolling second are dropped (see [`rate_limit_tick`]), not
/// buffered, so the connection never falls behind trying to catch up.
pub const OS_EVENTS_PUSH_CAP_PER_SEC: u32 = 20;

/// Build the live-push payload for one `AutopilotEvent`, shaped like an
/// `os.events.recent` row so the frontend applies it with zero conversion —
/// `{ event, ts, source, payload }`. Returns `None` for any non-OS event
/// variant (the broadcast channel also carries `TaskCreated` / `AgentIdle` /
/// etc.; this tail only forwards the two OS perception kinds).
///
/// Deliberately omits `id`: that only exists once
/// [`spawn_os_event_persistence`]'s async bridge has written the row to
/// `events.db`, and a live push races that write — there is no correct value
/// to put here. The frontend synthesizes its own list key for pushed rows.
pub fn os_event_push_payload(ev: &AutopilotEvent) -> Option<serde_json::Value> {
    match ev {
        AutopilotEvent::OsFileEvent { .. } | AutopilotEvent::OsFrontmostEvent { .. } => {
            Some(serde_json::json!({
                "event": ev.event_name(),
                "ts": chrono::Utc::now().to_rfc3339(),
                "source": crate::events_store::SOURCE_INTERNAL_BROADCAST,
                "payload": serde_json::Value::Object(ev.to_fields()),
            }))
        }
        _ => None,
    }
}

/// Pure sliding-1-second-window rate limiter tick for the per-connection live
/// tail (kept side-effect-free/time-source-free so it's unit testable without
/// real sleeps). All timestamps are caller-supplied milliseconds since some
/// fixed reference (the WS handler uses `Instant::elapsed` since connection
/// open) — only deltas matter, not absolute value.
///
/// Returns `(allow, new_window_start_ms, new_count)`. When `now_ms` has moved
/// ≥1000ms past `window_start_ms` the window rolls over (count resets to 0
/// before applying this tick); otherwise the count accumulates against `cap`.
pub fn rate_limit_tick(
    window_start_ms: u64,
    count: u32,
    cap: u32,
    now_ms: u64,
) -> (bool, u64, u32) {
    let (window_start_ms, count) = if now_ms.saturating_sub(window_start_ms) >= 1000 {
        (now_ms, 0)
    } else {
        (window_start_ms, count)
    };
    if count < cap {
        (true, window_start_ms, count + 1)
    } else {
        (false, window_start_ms, count)
    }
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

    #[test]
    fn read_goal_template_config_absent_is_none() {
        let dir = tempfile::tempdir().unwrap();
        // No agent.toml at all.
        assert!(read_goal_template_config(dir.path()).is_none());

        // agent.toml without [os_watch].
        std::fs::write(
            dir.path().join("agent.toml"),
            "[capabilities]\nos_native = true\n",
        )
        .unwrap();
        assert!(read_goal_template_config(dir.path()).is_none());

        // [os_watch] present but no goal_template key.
        std::fs::write(
            dir.path().join("agent.toml"),
            "[os_watch]\npaths = [\"/tmp\"]\n",
        )
        .unwrap();
        assert!(read_goal_template_config(dir.path()).is_none());

        // goal_template present but blank ⇒ never kick off by default.
        std::fs::write(
            dir.path().join("agent.toml"),
            "[os_watch]\ngoal_template = \"   \"\n",
        )
        .unwrap();
        assert!(read_goal_template_config(dir.path()).is_none());
    }

    #[test]
    fn read_goal_template_config_malformed_toml_is_none() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("agent.toml"), "not valid toml [[[").unwrap();
        assert!(read_goal_template_config(dir.path()).is_none());
    }

    #[test]
    fn read_goal_template_config_parses_template_only() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("agent.toml"),
            "[os_watch]\ngoal_template = \"整理 {file_name}（{kind}）到 {path}\"\n",
        )
        .unwrap();
        let cfg = read_goal_template_config(dir.path()).expect("should parse");
        assert_eq!(cfg.template, "整理 {file_name}（{kind}）到 {path}");
        assert_eq!(cfg.acceptance, None);
    }

    #[test]
    fn read_goal_template_config_parses_template_and_acceptance() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("agent.toml"),
            r#"
[os_watch]
goal_template = "整理 {file_name} 到月報"
goal_acceptance = "月報含 {file_name} 的資料"
"#,
        )
        .unwrap();
        let cfg = read_goal_template_config(dir.path()).expect("should parse");
        assert_eq!(cfg.template, "整理 {file_name} 到月報");
        assert_eq!(cfg.acceptance.as_deref(), Some("月報含 {file_name} 的資料"));
    }

    #[test]
    fn read_goal_template_config_trims_whitespace_and_drops_blank_acceptance() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("agent.toml"),
            "[os_watch]\ngoal_template = \"  do {path}  \"\ngoal_acceptance = \"   \"\n",
        )
        .unwrap();
        let cfg = read_goal_template_config(dir.path()).expect("should parse");
        assert_eq!(cfg.template, "do {path}");
        assert_eq!(cfg.acceptance, None);
    }

    #[test]
    fn partition_by_quota_none_allows_all() {
        let ids = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let r = partition_by_quota(ids.clone(), None);
        assert_eq!(r.allowed.len(), 3);
        assert!(r.skipped.is_empty());
        assert!(ids.iter().all(|i| r.allowed.contains(i)));
    }

    #[test]
    fn partition_by_quota_one_allows_stable_first_skips_rest() {
        // Sorted input → deterministic winner ("alice") across restarts.
        let ids = vec![
            "alice".to_string(),
            "bruno".to_string(),
            "carol".to_string(),
        ];
        let r = partition_by_quota(ids, Some(1));
        assert_eq!(r.allowed.len(), 1);
        assert!(r.allowed.contains("alice"));
        assert_eq!(r.skipped, vec!["bruno".to_string(), "carol".to_string()]);
    }

    #[test]
    fn partition_by_quota_under_capacity_skips_nothing() {
        let ids = vec!["a".to_string(), "b".to_string()];
        let r = partition_by_quota(ids, Some(5));
        assert_eq!(r.allowed.len(), 2);
        assert!(r.skipped.is_empty());
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

    // ── P4-1: os event persistence bridge ────────────────────

    #[tokio::test]
    async fn persists_os_file_event_with_internal_source_marker() {
        let home = tempfile::tempdir().unwrap();
        let events = Arc::new(crate::events_store::EventBusStore::open(home.path()).unwrap());
        let (tx, rx) = broadcast::channel::<AutopilotEvent>(16);
        let handle = spawn_os_event_persistence(events.clone(), rx);

        tx.send(AutopilotEvent::OsFileEvent {
            agent_id: "bruno".into(),
            path: "/inbox/report.pdf".into(),
            change: "created".into(),
        })
        .unwrap();

        // Give the subscriber task a moment to process the send.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let row = loop {
            let rows = events.fetch_since(0, 10).await.unwrap();
            if let Some(r) = rows.into_iter().next() {
                break r;
            }
            if tokio::time::Instant::now() > deadline {
                panic!("event was not persisted within 5s");
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        };

        assert_eq!(row.event, "os_file");
        assert_eq!(
            row.source.as_deref(),
            Some(crate::events_store::SOURCE_INTERNAL_BROADCAST)
        );
        let payload: serde_json::Value = serde_json::from_str(&row.payload).unwrap();
        assert_eq!(payload.get("agent_id").unwrap(), "bruno");
        assert_eq!(payload.get("path").unwrap(), "/inbox/report.pdf");
        assert_eq!(payload.get("kind").unwrap(), "created");

        handle.abort();
    }

    #[tokio::test]
    async fn persists_os_frontmost_event_with_internal_source_marker() {
        let home = tempfile::tempdir().unwrap();
        let events = Arc::new(crate::events_store::EventBusStore::open(home.path()).unwrap());
        let (tx, rx) = broadcast::channel::<AutopilotEvent>(16);
        let handle = spawn_os_event_persistence(events.clone(), rx);

        tx.send(AutopilotEvent::OsFrontmostEvent {
            agent_id: "bruno".into(),
            app: "Xcode".into(),
            window_title: "main.rs".into(),
            prev_app: "Finder".into(),
        })
        .unwrap();

        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let row = loop {
            let rows = events.fetch_since(0, 10).await.unwrap();
            if let Some(r) = rows.into_iter().next() {
                break r;
            }
            if tokio::time::Instant::now() > deadline {
                panic!("event was not persisted within 5s");
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        };

        assert_eq!(row.event, "os_frontmost");
        assert_eq!(
            row.source.as_deref(),
            Some(crate::events_store::SOURCE_INTERNAL_BROADCAST)
        );
        let payload: serde_json::Value = serde_json::from_str(&row.payload).unwrap();
        assert_eq!(payload.get("app").unwrap(), "Xcode");

        handle.abort();
    }

    /// End-to-end proof of the P4-1 integration-gap closure: an os_file event
    /// that only ever crosses the in-process broadcast (never a direct
    /// `events.db` write) is persisted by the bridge, and
    /// `rule_induction::detect_patterns` — reading purely via
    /// `EventBusStore::fetch_since`, the same API `RuleInductor::run_once`
    /// uses — finds the pattern once enough reacted occurrences accumulate.
    #[tokio::test]
    async fn bridged_events_are_visible_to_rule_inductor_pattern_detection() {
        let home = tempfile::tempdir().unwrap();
        let events = Arc::new(crate::events_store::EventBusStore::open(home.path()).unwrap());
        let (tx, rx) = broadcast::channel::<AutopilotEvent>(16);
        let handle = spawn_os_event_persistence(events.clone(), rx);

        // 5 perception+reaction pairs, matching rule_induction's default
        // min_occurrences=5. Reactions are written directly (as the MCP
        // subprocess would) — only the perception side goes through the
        // broadcast bridge under test.
        for _ in 0..5 {
            tx.send(AutopilotEvent::OsFileEvent {
                agent_id: "bruno".into(),
                path: "/inbox/report.pdf".into(),
                change: "created".into(),
            })
            .unwrap();
        }

        // Wait for all 5 to land.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let rows = events.fetch_since(0, 100).await.unwrap();
            if rows.len() >= 5 {
                break;
            }
            if tokio::time::Instant::now() > deadline {
                panic!("events were not all persisted within 5s");
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        // Write 5 reactions, each shortly after its perception row (direct
        // write, as `append_bus_event` in the MCP subprocess would do).
        let rows = events.fetch_since(0, 100).await.unwrap();
        for r in &rows {
            let ts = chrono::DateTime::parse_from_rfc3339(&r.ts)
                .unwrap()
                .with_timezone(&chrono::Utc)
                + chrono::Duration::seconds(10);
            events
                .append_with_ts(
                    "task.created",
                    &serde_json::json!({ "assigned_to": "bruno" }).to_string(),
                    &ts.to_rfc3339(),
                )
                .await
                .unwrap();
        }

        let all_rows = events.fetch_since(0, 1000).await.unwrap();
        let patterns = crate::rule_induction::detect_patterns(
            &all_rows,
            chrono::Utc::now(),
            &crate::rule_induction::RuleInductionConfig::default(),
        );
        assert_eq!(patterns.len(), 1);
        assert_eq!(patterns[0].agent_id, "bruno");
        assert_eq!(patterns[0].dimension_key, "pdf");
        assert_eq!(patterns[0].occurrences, 5);

        handle.abort();
    }

    // ── P4-3+: live dashboard event tail ──────────────────────

    #[test]
    fn os_event_push_payload_forwards_os_file_and_os_frontmost() {
        let file_ev = AutopilotEvent::OsFileEvent {
            agent_id: "bruno".into(),
            path: "/inbox/report.pdf".into(),
            change: "created".into(),
        };
        let p = os_event_push_payload(&file_ev).expect("os_file should forward");
        assert_eq!(p["event"].as_str(), Some("os_file"));
        assert_eq!(
            p["source"].as_str(),
            Some(crate::events_store::SOURCE_INTERNAL_BROADCAST)
        );
        assert!(p.get("id").is_none(), "live push must not carry a DB id");
        assert_eq!(p["payload"]["agent_id"].as_str(), Some("bruno"));
        assert!(p["ts"].as_str().is_some());

        let frontmost_ev = AutopilotEvent::OsFrontmostEvent {
            agent_id: "bruno".into(),
            app: "Xcode".into(),
            window_title: "main.rs".into(),
            prev_app: "Finder".into(),
        };
        let p2 = os_event_push_payload(&frontmost_ev).expect("os_frontmost should forward");
        assert_eq!(p2["event"].as_str(), Some("os_frontmost"));
        assert_eq!(p2["payload"]["app"].as_str(), Some("Xcode"));
    }

    #[test]
    fn os_event_push_payload_ignores_non_os_events() {
        let ev = AutopilotEvent::AgentIdle {
            agent_id: "bruno".into(),
            idle_minutes: 5,
        };
        assert!(os_event_push_payload(&ev).is_none());
    }

    #[test]
    fn rate_limit_tick_allows_up_to_cap_then_drops_within_same_window() {
        let cap = 3;
        let mut window_start = 0u64;
        let mut count = 0u32;
        for i in 0..cap {
            let (allow, ws, c) = rate_limit_tick(window_start, count, cap, 100 + i as u64);
            assert!(allow, "event {i} should be allowed");
            window_start = ws;
            count = c;
        }
        assert_eq!(count, cap);
        // The (cap+1)-th event in the SAME window is dropped.
        let (allow, ws, c) = rate_limit_tick(window_start, count, cap, 100 + cap as u64);
        assert!(!allow, "over-cap event should be dropped");
        assert_eq!(ws, window_start, "window unchanged on drop");
        assert_eq!(c, cap, "count unchanged on drop");
    }

    #[test]
    fn rate_limit_tick_resets_after_1s_window_rolls_over() {
        let cap = 2;
        // Exhaust the first window.
        let (a1, ws1, c1) = rate_limit_tick(0, 0, cap, 0);
        assert!(a1);
        let (a2, ws2, c2) = rate_limit_tick(ws1, c1, cap, 50);
        assert!(a2);
        let (a3, ws3, c3) = rate_limit_tick(ws2, c2, cap, 60);
        assert!(!a3, "cap exhausted within the same window");
        // 1000ms later, a fresh window starts and the tick is allowed again.
        let (a4, ws4, c4) = rate_limit_tick(ws3, c3, cap, ws3 + 1000);
        assert!(a4, "new window should reset the cap");
        assert_eq!(ws4, ws3 + 1000);
        assert_eq!(c4, 1);
    }

    #[test]
    fn rate_limit_tick_zero_cap_always_drops() {
        let (allow, _ws, count) = rate_limit_tick(0, 0, 0, 0);
        assert!(!allow);
        assert_eq!(count, 0);
    }
}
