//! OS-native P2-4: frontmost (foreground) app/window polling → autopilot bus.
//!
//! At gateway startup [`init_frontmost_polling`] scans the agent registry and,
//! for every agent with `[capabilities] os_native = true` **and** a positive
//! `[os_watch] frontmost_poll_secs`, spawns a low-frequency poll loop that
//! calls `duduclaw_os::frontmost_info()` and forwards a change onto the same
//! `broadcast::Sender<AutopilotEvent>` the filesystem watchers use
//! (`os_events.rs`), as [`AutopilotEvent::OsFrontmostEvent`]. An event is only
//! emitted when the app or window title actually changed since the previous
//! poll — a steady-state poll that observes no change is silent (no event
//! spam from an idle desktop).
//!
//! **P2 implementation rule #1 (AgentIdle single judgment source)**: this
//! module is a pure sensing source. It does not compute, report, or influence
//! idle state — idle stays computed solely by the existing heartbeat path. Any
//! rule author wanting "notify me when idle AND frontmost changed" combines
//! `agent_idle` and `os_frontmost` rules themselves; this module never opens a
//! second idle channel.
//!
//! No agent is polled by default (0 / absent `frontmost_poll_secs`), mirroring
//! the `[os_watch] paths` opt-in convention — polling frontmost state is a
//! deliberate, per-agent choice, not a default-on behavior.

use std::collections::HashMap;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{Mutex, RwLock, broadcast};
use tokio::task::JoinHandle;
use tracing::{info, warn};

use duduclaw_agent::registry::AgentRegistry;
use duduclaw_os::frontmost::{FrontmostError, frontmost_info};

use crate::autopilot_engine::AutopilotEvent;

/// Read `[os_watch] frontmost_poll_secs` from an agent's `agent.toml`.
///
/// Goes through the shared typed parse point
/// ([`duduclaw_core::agent_toml`]) — the key is typed on
/// [`duduclaw_core::types::OsWatchSection`], so it is now visible to
/// `AgentConfig` (and survives the `agent_update` round-trip) while keeping
/// the tolerance of the raw walk it replaces. Returns `None` (do not poll)
/// when the file/table/key is absent, malformed, non-positive, or the value
/// doesn't parse as an integer. `0` is an explicit "disabled" value, not an
/// error.
pub fn read_frontmost_poll_secs(agent_dir: &Path) -> Option<u64> {
    let secs = duduclaw_core::agent_toml::load(agent_dir)
        .os_watch
        .frontmost_poll_secs?;
    if secs <= 0 { None } else { Some(secs as u64) }
}

/// `<home>/os/<agent>/` — durable per-agent OS observation logs.
pub fn frontmost_log_dir(home_dir: &Path, agent_id: &str) -> PathBuf {
    home_dir.join("os").join(agent_id)
}

/// Today's frontmost log filename (local days — the summary is user-facing).
pub fn frontmost_log_name(date: chrono::NaiveDate) -> String {
    format!("frontmost-{}.jsonl", date.format("%Y-%m-%d"))
}

/// Append one app-switch line `{"ts","app"}` to today's daily log.
///
/// Data minimization: the **window title is never written** — only the app
/// name + timestamp, the minimum the proactive-care summary needs. On the
/// first write of a new day, older day-files (keeping today + yesterday) are
/// pruned. Errors are swallowed after a warn — losing a log line must never
/// affect the poll loop.
pub fn append_frontmost_log(home_dir: &Path, agent_id: &str, app: &str) {
    let dir = frontmost_log_dir(home_dir, agent_id);
    let today = chrono::Local::now().date_naive();
    let path = dir.join(frontmost_log_name(today));
    if !path.exists() {
        if let Err(e) = std::fs::create_dir_all(&dir) {
            warn!(agent = %agent_id, error = %e, "frontmost log dir create failed");
            return;
        }
        // New day — prune everything except today and yesterday.
        let keep: HashSet<String> = [
            frontmost_log_name(today),
            frontmost_log_name(today.pred_opt().unwrap_or(today)),
        ]
        .into_iter()
        .collect();
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for entry in rd.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.starts_with("frontmost-") && !keep.contains(&name) {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }
    }
    let line = serde_json::json!({
        "ts": chrono::Local::now().to_rfc3339(),
        "app": duduclaw_core::truncate_chars(app, 80),
    });
    let res = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut f| {
            use std::io::Write;
            writeln!(f, "{line}")
        });
    if let Err(e) = res {
        warn!(agent = %agent_id, error = %e, "frontmost log append failed");
    }
}

/// One polling cycle's outcome, kept in the loop's local state.
struct PollState {
    /// Last successfully observed (app, window_title); `None` before the
    /// first successful poll.
    last: Option<(String, String)>,
    /// The most recent error message, so a persistently-failing poll (e.g.
    /// TCC permission never granted, or an unsupported platform) logs once
    /// instead of spamming on every tick. Cleared on the next success.
    last_error: Option<String>,
}

/// Spawn the poll loop for one agent. The returned [`JoinHandle`] is OWNED by
/// the [`OsFrontmostRegistry`] so an `agents.update` edit can abort it in place
/// (P4-3 hot reload — the "future pass" the original P2-4 doc anticipated).
fn spawn_agent_poll(
    agent_id: String,
    poll_secs: u64,
    tx: broadcast::Sender<AutopilotEvent>,
    home_dir: Option<PathBuf>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut state = PollState {
            last: None,
            last_error: None,
        };
        let mut ticker = tokio::time::interval(Duration::from_secs(poll_secs));
        // The first tick fires immediately; that's fine — it just seeds
        // `state.last` a few seconds earlier than the second tick would.
        loop {
            ticker.tick().await;
            match frontmost_info().await {
                Ok(info) => {
                    if state.last_error.take().is_some() {
                        info!(agent = %agent_id, "frontmost polling recovered");
                    }
                    let changed = state
                        .last
                        .as_ref()
                        .is_none_or(|(app, title)| *app != info.app || *title != info.window_title);
                    if changed {
                        let prev_app = state
                            .last
                            .as_ref()
                            .map(|(app, _)| app.clone())
                            .unwrap_or_default();
                        // A send error only means there are currently no
                        // autopilot subscribers — safe to ignore (same
                        // tolerance as the filesystem watcher forwarder).
                        let _ = tx.send(AutopilotEvent::OsFrontmostEvent {
                            agent_id: agent_id.clone(),
                            app: info.app.clone(),
                            window_title: info.window_title.clone(),
                            prev_app,
                        });
                        // Durable daily app-switch log — the data source the
                        // proactive-care check summarises (survives restarts,
                        // unlike the footprint distiller's in-memory state).
                        if let Some(home) = home_dir.clone() {
                            let aid = agent_id.clone();
                            let app = info.app.clone();
                            let _ = tokio::task::spawn_blocking(move || {
                                append_frontmost_log(&home, &aid, &app);
                            })
                            .await;
                        }
                        state.last = Some((info.app, info.window_title));
                    }
                }
                Err(e) => {
                    // `Unsupported` on a non-macOS/xdotool-less host is an
                    // expected, permanent condition — stop polling rather than
                    // looping forever on a call that can never succeed.
                    if matches!(e, FrontmostError::Unsupported) {
                        warn!(
                            agent = %agent_id,
                            "frontmost polling stopped: not supported on this platform"
                        );
                        return;
                    }
                    let msg = e.to_string();
                    if state.last_error.as_deref() != Some(msg.as_str()) {
                        warn!(agent = %agent_id, error = %msg, "frontmost poll failed");
                        state.last_error = Some(msg);
                    }
                }
            }
        }
    })
}

/// One running frontmost poll task, keyed by agent id in the
/// [`OsFrontmostRegistry`]. Holds the poll `JoinHandle` (abort → stop) plus a
/// snapshot of the effective `poll_secs` for the `os.status` RPC.
struct FrontmostEntry {
    poll: JoinHandle<()>,
    poll_secs: u64,
}

/// Shared registry of running per-agent frontmost poll tasks. Held in
/// `AppState` so the `agents.update` / `os.settings.update` RPCs can hot
/// stop/start one agent's polling after a `frontmost_poll_secs` / `os_native`
/// edit — no gateway restart. Symmetric with `os_events::OsWatcherRegistry`
/// (the P4-3 closure of the P2-4 "future pass" note).
pub struct OsFrontmostRegistry {
    tasks: Mutex<HashMap<String, FrontmostEntry>>,
}

impl Default for OsFrontmostRegistry {
    fn default() -> Self {
        Self {
            tasks: Mutex::new(HashMap::new()),
        }
    }
}

impl OsFrontmostRegistry {
    /// Create an empty registry.
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Start (or restart) frontmost polling for one agent from its current
    /// `[os_watch] frontmost_poll_secs`, forwarding change events onto `tx`.
    /// Any existing poll task for the agent is stopped first. Returns `true`
    /// iff a poll task is now running — a missing/zero `frontmost_poll_secs`
    /// returns `false` (no entry registered). The caller is responsible for
    /// the `os_native` capability + quota gate; this method only reads the
    /// poll interval.
    pub async fn start_agent(
        &self,
        agent_id: &str,
        agent_dir: &Path,
        tx: broadcast::Sender<AutopilotEvent>,
    ) -> bool {
        self.stop_agent(agent_id).await;
        let Some(secs) = read_frontmost_poll_secs(agent_dir) else {
            return false;
        };
        info!(agent = %agent_id, poll_secs = secs, "starting frontmost polling");
        // `<home>/agents/<id>` → `<home>` (for the daily app-switch log).
        let home_dir = agent_dir.parent().and_then(|p| p.parent()).map(Path::to_path_buf);
        let poll = spawn_agent_poll(agent_id.to_string(), secs, tx, home_dir);
        self.tasks.lock().await.insert(
            agent_id.to_string(),
            FrontmostEntry {
                poll,
                poll_secs: secs,
            },
        );
        true
    }

    /// Stop and deregister one agent's poll task (abort → the loop's next
    /// `.await` is cancelled). Returns whether one was running.
    pub async fn stop_agent(&self, agent_id: &str) -> bool {
        if let Some(entry) = self.tasks.lock().await.remove(agent_id) {
            entry.poll.abort();
            true
        } else {
            false
        }
    }

    /// Whether a poll task is currently running for the agent, and its
    /// effective interval (`None` when not polling). Used by `os.status`.
    pub async fn status(&self, agent_id: &str) -> Option<u64> {
        self.tasks.lock().await.get(agent_id).map(|e| e.poll_secs)
    }

    /// Number of running poll tasks (test / status helper).
    pub async fn len(&self) -> usize {
        self.tasks.lock().await.len()
    }

    /// True when no poll task is currently running.
    pub async fn is_empty(&self) -> bool {
        self.tasks.lock().await.is_empty()
    }
}

/// Scan the registry and start a poll task per eligible agent into `registry`.
///
/// `agent_id` keys use the agent's **directory name**, matching the
/// `os_events::init_os_watchers` convention so rule authors see the same
/// identifier across `os_file` and `os_frontmost` events. `allowed` is the
/// quota-resolved set of dir-name ids permitted to run OS-native features
/// (see `os_events::resolve_os_native_allowed`); an agent outside it is
/// skipped even when it declares `frontmost_poll_secs`.
pub async fn init_frontmost_polling(
    registry: Arc<OsFrontmostRegistry>,
    agent_registry: Arc<RwLock<AgentRegistry>>,
    tx: broadcast::Sender<AutopilotEvent>,
    allowed: &HashSet<String>,
) {
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
        info!(
            "no agents configured with [os_watch] frontmost_poll_secs — frontmost polling not started"
        );
        return;
    }

    for (agent_id, agent_dir) in candidates {
        registry
            .start_agent(&agent_id, &agent_dir, tx.clone())
            .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── R5: `[os_watch] frontmost_poll_secs` direction, pinned ───────────
    //
    // absent / malformed / wrong-typed / non-positive ⇒ None ⇒ DO NOT POLL.
    // Polling reads the user's frontmost application, so "we couldn't read
    // the config" must never resolve to "start observing anyway"; `0` is an
    // explicit disable, not an error. Note this is the OPPOSITE convention
    // from `[capabilities] grant_ttl_secs`, where `0` means "use the
    // default" — same-shaped key, deliberately different direction.

    #[test]
    fn default_direction_frontmost_poll_never_polls_on_a_bad_config() {
        for body in [
            "",                                        // empty file
            "[os_watch]\n",                            // section, no key
            "[os_watch]\nfrontmost_poll_secs = 0\n",   // explicit disable
            "[os_watch]\nfrontmost_poll_secs = -5\n",  // negative
            "[os_watch]\nfrontmost_poll_secs = \"60\"\n", // wrong type
            "[os_watch]\nfrontmost_poll_secs = 60.5\n",   // float ⇒ not an int
            "os_watch = \"scalar\"\n",                 // wrong-typed section
            "not valid toml [[[",                      // malformed file
        ] {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(dir.path().join("agent.toml"), body).unwrap();
            assert!(
                read_frontmost_poll_secs(dir.path()).is_none(),
                "must not poll for {body:?}"
            );
        }

        // A well-formed positive value is the ONLY thing that starts polling.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("agent.toml"),
            "[os_watch]\nfrontmost_poll_secs = 90\n",
        )
        .unwrap();
        assert_eq!(read_frontmost_poll_secs(dir.path()), Some(90));
    }

    #[test]
    fn read_frontmost_poll_secs_absent_is_none() {
        let dir = tempfile::tempdir().unwrap();
        // No agent.toml at all.
        assert!(read_frontmost_poll_secs(dir.path()).is_none());

        // agent.toml without [os_watch].
        std::fs::write(
            dir.path().join("agent.toml"),
            "[capabilities]\nos_native = true\n",
        )
        .unwrap();
        assert!(read_frontmost_poll_secs(dir.path()).is_none());

        // [os_watch] present but no frontmost_poll_secs key.
        std::fs::write(
            dir.path().join("agent.toml"),
            "[os_watch]\npaths = [\"/tmp\"]\n",
        )
        .unwrap();
        assert!(read_frontmost_poll_secs(dir.path()).is_none());
    }

    #[test]
    fn read_frontmost_poll_secs_zero_is_none() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("agent.toml"),
            "[os_watch]\nfrontmost_poll_secs = 0\n",
        )
        .unwrap();
        assert!(read_frontmost_poll_secs(dir.path()).is_none());
    }

    #[test]
    fn read_frontmost_poll_secs_positive_value_parses() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("agent.toml"),
            "[os_watch]\nfrontmost_poll_secs = 30\n",
        )
        .unwrap();
        assert_eq!(read_frontmost_poll_secs(dir.path()), Some(30));
    }

    #[test]
    fn read_frontmost_poll_secs_negative_is_none() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("agent.toml"),
            "[os_watch]\nfrontmost_poll_secs = -5\n",
        )
        .unwrap();
        assert!(read_frontmost_poll_secs(dir.path()).is_none());
    }

    #[test]
    fn read_frontmost_poll_secs_malformed_toml_is_none() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("agent.toml"), "not valid toml [[[").unwrap();
        assert!(read_frontmost_poll_secs(dir.path()).is_none());
    }

    #[tokio::test]
    async fn init_frontmost_polling_empty_registry_starts_nothing() {
        let agents_dir = tempfile::tempdir().unwrap();
        let agent_registry = Arc::new(RwLock::new(AgentRegistry::new(
            agents_dir.path().to_path_buf(),
        )));
        let (tx, _rx) = broadcast::channel::<AutopilotEvent>(16);
        let reg = OsFrontmostRegistry::new();
        init_frontmost_polling(reg.clone(), agent_registry, tx, &HashSet::new()).await;
        assert!(reg.is_empty().await);
    }

    #[tokio::test]
    async fn registry_start_restart_stop_agent() {
        let agent_dir = tempfile::tempdir().unwrap();
        std::fs::write(
            agent_dir.path().join("agent.toml"),
            "[os_watch]\nfrontmost_poll_secs = 60\n",
        )
        .unwrap();
        let reg = OsFrontmostRegistry::new();
        let (tx, _rx) = broadcast::channel::<AutopilotEvent>(16);

        // Start → one poll task registered, status reports the interval.
        assert!(reg.start_agent("a1", agent_dir.path(), tx.clone()).await);
        assert_eq!(reg.len().await, 1);
        assert_eq!(reg.status("a1").await, Some(60));

        // Restart in place (config edit) → still exactly one entry.
        assert!(reg.start_agent("a1", agent_dir.path(), tx.clone()).await);
        assert_eq!(reg.len().await, 1);

        // Stop → deregistered; second stop is a no-op.
        assert!(reg.stop_agent("a1").await);
        assert!(reg.is_empty().await);
        assert_eq!(reg.status("a1").await, None);
        assert!(!reg.stop_agent("a1").await);

        // frontmost_poll_secs = 0 → not started, prior task cleared.
        assert!(reg.start_agent("a1", agent_dir.path(), tx.clone()).await);
        assert_eq!(reg.len().await, 1);
        std::fs::write(
            agent_dir.path().join("agent.toml"),
            "[os_watch]\nfrontmost_poll_secs = 0\n",
        )
        .unwrap();
        assert!(!reg.start_agent("a1", agent_dir.path(), tx).await);
        assert!(reg.is_empty().await);
    }

    #[test]
    fn frontmost_log_appends_and_prunes_old_days() {
        let home = tempfile::tempdir().unwrap();
        let dir = frontmost_log_dir(home.path(), "a1");
        std::fs::create_dir_all(&dir).unwrap();
        // A stale day-file from last week must be pruned on the first write
        // of a day (today's file does not exist yet).
        let stale = dir.join("frontmost-2020-01-01.jsonl");
        std::fs::write(&stale, "{}\n").unwrap();

        append_frontmost_log(home.path(), "a1", "Safari");
        append_frontmost_log(home.path(), "a1", "Xcode");

        assert!(!stale.exists(), "stale day-file pruned");
        let today = chrono::Local::now().date_naive();
        let content =
            std::fs::read_to_string(dir.join(frontmost_log_name(today))).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 2);
        let first: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(first["app"], "Safari");
        assert!(first["ts"].as_str().unwrap().len() > 10);
        // Data minimization: no window-title key exists in the schema.
        assert!(first.get("window_title").is_none());
    }
}
