use std::collections::HashMap;
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
}

impl HeartbeatScheduler {
    /// Create a new empty scheduler.
    pub fn new() -> Self {
        Self {
            tasks: Arc::new(RwLock::new(HashMap::new())),
            running: Arc::new(AtomicBool::new(false)),
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
                    // TODO: trigger agent heartbeat action (Phase 4)
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
