//! Cron task scheduler — reads `cron_tasks.jsonl`, evaluates cron expressions,
//! and executes due tasks by calling the Claude CLI for the target agent.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::Utc;
use cron::Schedule;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::claude_runner::call_claude_for_agent;
use duduclaw_agent::registry::AgentRegistry;

/// A single persisted cron task entry from `cron_tasks.jsonl`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CronTask {
    pub id: String,
    #[serde(default = "default_name")]
    pub name: String,
    #[serde(default = "default_agent")]
    pub agent_id: String,
    pub cron: String,
    #[serde(alias = "description")]
    pub task: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub created_at: Option<String>,
}

fn default_name() -> String {
    "unnamed".to_string()
}
fn default_agent() -> String {
    "default".to_string()
}
fn default_true() -> bool {
    true
}

/// In-memory representation with parsed schedule and last-run tracking.
struct LiveTask {
    task: CronTask,
    schedule: Schedule,
    last_run: Option<chrono::DateTime<Utc>>,
}

/// Cron scheduler that loads tasks from `cron_tasks.jsonl` and fires them on time.
/// Maximum concurrent cron task executions.
const MAX_CONCURRENT_CRON: usize = 4;

pub struct CronScheduler {
    home_dir: PathBuf,
    registry: Arc<RwLock<AgentRegistry>>,
    tasks: Arc<RwLock<Vec<LiveTask>>>,
    semaphore: Arc<tokio::sync::Semaphore>,
}

impl CronScheduler {
    pub fn new(home_dir: PathBuf, registry: Arc<RwLock<AgentRegistry>>) -> Self {
        Self {
            home_dir,
            registry,
            tasks: Arc::new(RwLock::new(Vec::new())),
            semaphore: Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_CRON)),
        }
    }

    /// Load tasks from `cron_tasks.jsonl`. Skips invalid lines gracefully.
    async fn load_tasks(&self) -> Vec<CronTask> {
        let path = self.home_dir.join("cron_tasks.jsonl");
        let content = match tokio::fs::read_to_string(&path).await {
            Ok(c) => c,
            Err(_) => return Vec::new(),
        };

        content
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|line| {
                serde_json::from_str::<CronTask>(line)
                    .map_err(|e| warn!("Skipping invalid cron task line: {e}"))
                    .ok()
            })
            .filter(|t| t.enabled)
            .collect()
    }

    /// Reload tasks from disk into memory, merging with existing last_run state.
    async fn reload(&self) {
        let raw_tasks = self.load_tasks().await;
        let mut live = self.tasks.write().await;

        // Preserve last_run for tasks that already existed
        let old_runs: std::collections::HashMap<String, chrono::DateTime<Utc>> = live
            .iter()
            .filter_map(|lt| lt.last_run.map(|lr| (lt.task.id.clone(), lr)))
            .collect();

        let mut new_live = Vec::new();
        for task in raw_tasks {
            // The `cron` crate expects 6-field or 7-field expressions.
            // Normalise 5-field (standard) to 6-field by prepending "0" for seconds.
            let expr = normalise_cron(&task.cron);
            match expr.parse::<Schedule>() {
                Ok(schedule) => {
                    let last_run = old_runs.get(&task.id).copied();
                    new_live.push(LiveTask {
                        task,
                        schedule,
                        last_run,
                    });
                }
                Err(e) => {
                    warn!(id = %task.id, cron = %task.cron, "Invalid cron expression: {e}");
                }
            }
        }

        info!(count = new_live.len(), "Cron tasks loaded");
        *live = new_live;
    }

    /// Start the scheduler loop. Checks every 30 seconds and reloads from disk
    /// every 5 minutes to pick up new tasks added by `schedule_task`.
    pub async fn run(self: Arc<Self>) {
        self.reload().await;

        let mut tick = 0u64;
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            tick += 1;

            // Reload from disk every 5 minutes (10 ticks * 30s)
            if tick % 10 == 0 {
                self.reload().await;
            }

            let now = Utc::now();

            // Collect tasks to spawn while holding write lock, then release before spawning (BE-M2)
            let mut to_spawn = Vec::new();
            {
                let mut tasks = self.tasks.write().await;
                for lt in tasks.iter_mut() {
                    let should_fire = match lt.last_run {
                        Some(last) => {
                            lt.schedule
                                .after(&last)
                                .next()
                                .map(|next| next <= now)
                                .unwrap_or(false)
                        }
                        None => {
                            lt.schedule
                                .after(&(now - chrono::Duration::hours(1)))
                                .next()
                                .map(|next| next <= now)
                                .unwrap_or(false)
                        }
                    };

                    if should_fire {
                        info!(
                            id = %lt.task.id,
                            name = %lt.task.name,
                            agent = %lt.task.agent_id,
                            "Cron task firing"
                        );
                        lt.last_run = Some(now);
                        to_spawn.push(lt.task.clone());
                    }
                }
            } // write lock released

            for task in to_spawn {
                let home = self.home_dir.clone();
                let registry = self.registry.clone();
                let sem = self.semaphore.clone();
                tokio::spawn(async move {
                    let _permit = sem.acquire().await;
                    execute_cron_task(&home, &registry, &task).await;
                });
            }
        }
    }
}

/// Execute a cron task by calling the Claude CLI for the target agent.
async fn execute_cron_task(
    home_dir: &Path,
    registry: &Arc<RwLock<AgentRegistry>>,
    task: &CronTask,
) {
    let prompt = format!(
        "[Scheduled Task: {}] {}",
        task.name, task.task
    );

    match call_claude_for_agent(home_dir, registry, &task.agent_id, &prompt).await {
        Ok(response) => {
            info!(
                id = %task.id,
                name = %task.name,
                response_len = response.len(),
                "Cron task completed"
            );
        }
        Err(e) => {
            warn!(id = %task.id, name = %task.name, "Cron task failed: {e}");
        }
    }
}

/// Normalise a cron expression to 6-field format (with seconds).
/// If the expression has 5 fields, prepend "0" for seconds.
fn normalise_cron(expr: &str) -> String {
    let fields: Vec<&str> = expr.split_whitespace().collect();
    if fields.len() == 5 {
        format!("0 {expr}")
    } else {
        expr.to_string()
    }
}

/// Start the cron scheduler as a background task.
pub fn start_cron_scheduler(
    home_dir: PathBuf,
    registry: Arc<RwLock<AgentRegistry>>,
) -> tokio::task::JoinHandle<()> {
    let scheduler = Arc::new(CronScheduler::new(home_dir, registry));
    tokio::spawn(async move {
        scheduler.run().await;
    })
}
