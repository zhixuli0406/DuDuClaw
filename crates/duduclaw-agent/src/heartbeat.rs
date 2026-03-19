use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use chrono::{self, Utc};
use duduclaw_core::error::Result;
use duduclaw_core::types::HeartbeatConfig;
use tokio::sync::RwLock;
use tokio::time;
use tracing::{info, warn};

/// A registered heartbeat task for a single agent.
pub struct HeartbeatTask {
    pub agent_id: String,
    pub config: HeartbeatConfig,
    pub last_run: Option<chrono::DateTime<Utc>>,
}

/// Scheduler that periodically fires heartbeat actions for registered agents.
pub struct HeartbeatScheduler {
    tasks: Arc<RwLock<HashMap<String, HeartbeatTask>>>,
    running: Arc<AtomicBool>,
    /// Home directory used to locate the Python SDK for evolution calls.
    home_dir: PathBuf,
}

impl HeartbeatScheduler {
    /// Create a new empty scheduler.
    pub fn new() -> Self {
        Self {
            tasks: Arc::new(RwLock::new(HashMap::new())),
            running: Arc::new(AtomicBool::new(false)),
            home_dir: PathBuf::new(),
        }
    }

    /// Create a new scheduler with a home directory for evolution calls.
    pub fn with_home(home_dir: PathBuf) -> Self {
        Self {
            tasks: Arc::new(RwLock::new(HashMap::new())),
            running: Arc::new(AtomicBool::new(false)),
            home_dir,
        }
    }

    /// Register an agent's heartbeat.
    pub async fn register(&self, agent_id: &str, config: HeartbeatConfig) {
        info!(agent_id, interval = config.interval_seconds, "Registering heartbeat");

        self.tasks.write().await.insert(
            agent_id.to_string(),
            HeartbeatTask {
                agent_id: agent_id.to_string(),
                config,
                last_run: None,
            },
        );
    }

    /// Unregister an agent's heartbeat.
    pub async fn unregister(&self, agent_id: &str) {
        info!(agent_id, "Unregistering heartbeat");
        self.tasks.write().await.remove(agent_id);
    }

    /// Start the scheduler loop.
    ///
    /// This runs indefinitely until [`stop`](Self::stop) is called,
    /// checking every 30 seconds whether any heartbeat tasks are due.
    pub async fn run(&self) -> Result<()> {
        self.running.store(true, Ordering::SeqCst);
        info!(task_count = self.tasks.read().await.len(), "Heartbeat scheduler started");

        while self.running.load(Ordering::SeqCst) {
            let now = Utc::now();

            let mut tasks = self.tasks.write().await;
            for (agent_id, task) in tasks.iter_mut() {
                if !task.config.enabled {
                    continue;
                }

                let interval = Duration::from_secs(task.config.interval_seconds);
                let should_run = match task.last_run {
                    Some(last) => {
                        let elapsed = now.signed_duration_since(last);
                        elapsed >= chrono::Duration::from_std(interval).unwrap_or_default()
                    }
                    None => true, // never run before
                };

                if should_run {
                    info!(agent_id = %agent_id, "Heartbeat firing for agent");
                    task.last_run = Some(now);

                    // Trigger meso reflection for this agent in background
                    let aid = agent_id.clone();
                    let home = self.home_dir.clone();
                    let agent_dir = home.join("agents").join(&aid);
                    tokio::spawn(async move {
                        trigger_meso_reflection(&home, &aid, &agent_dir).await;
                    });
                }
            }
            drop(tasks);

            // Check every 30 seconds
            time::sleep(Duration::from_secs(30)).await;
        }

        warn!("Heartbeat scheduler stopped");
        Ok(())
    }

    /// Stop the scheduler.
    pub fn stop(&self) {
        info!("Stopping heartbeat scheduler");
        self.running.store(false, Ordering::SeqCst);
    }

    /// Get list of registered task agent IDs.
    pub async fn list_task_ids(&self) -> Vec<String> {
        self.tasks.read().await.keys().cloned().collect()
    }
}

impl Default for HeartbeatScheduler {
    fn default() -> Self {
        Self::new()
    }
}

// ── Evolution helper ─────────────────────────────────────────

async fn trigger_meso_reflection(home_dir: &Path, agent_id: &str, agent_dir: &Path) {
    let python_path = find_python_path(home_dir);
    let mut cmd = tokio::process::Command::new("python3");
    cmd.args([
        "-m", "duduclaw.evolution.run",
        "meso",
        "--agent-id", agent_id,
        "--agent-dir", &agent_dir.to_string_lossy(),
    ]);
    cmd.env("PYTHONPATH", &python_path);
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    match tokio::time::timeout(std::time::Duration::from_secs(60), cmd.output()).await {
        Ok(Ok(out)) if out.status.success() => {
            info!(agent_id, "Heartbeat meso reflection completed");
        }
        Ok(Ok(out)) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            warn!(agent_id, "Heartbeat meso reflection failed: {}", &stderr[..stderr.len().min(200)]);
        }
        Ok(Err(e)) => warn!(agent_id, "Heartbeat spawn error: {e}"),
        Err(_) => warn!(agent_id, "Heartbeat meso reflection timed out (60s)"),
    }
}

fn find_python_path(home_dir: &Path) -> String {
    let candidate = home_dir.parent().unwrap_or(home_dir).join("python");
    if candidate.join("duduclaw").exists() {
        return candidate.to_string_lossy().to_string();
    }
    std::env::var("PYTHONPATH").unwrap_or_default()
}
