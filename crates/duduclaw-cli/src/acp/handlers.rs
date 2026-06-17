//! ACP session handlers — maps ACP/A2A protocol operations to the DuDuClaw
//! agent runner.
//!
//! RFC-25 Phase 3: `handle_prompt_with_agent` now executes a REAL agent through
//! the gateway's provider-aware delegation path (`call_claude_for_agent_with_type`),
//! so an A2A `tasks/send` runs the target agent on its configured `[runtime]`
//! provider (Claude / Codex / Gemini / OpenAI-compat) — not a placeholder.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use tokio::sync::{Mutex, RwLock};
use tracing::info;

use duduclaw_agent::registry::AgentRegistry;

use super::types::*;

/// Shared per-`home_dir` agent-registry snapshot for A2A (RFC-25 R3 + followup).
///
/// Every `tasks/send` previously rebuilt the registry and re-scanned the agents
/// directory. A2A is low-frequency, but caching the snapshot avoids the repeated
/// filesystem scan. The cache is invalidated when the agents directory's mtime
/// changes (a subdir added/removed bumps the parent mtime), so a long-running
/// ACP server that gains or loses an agent picks it up on the next call without a
/// restart. Edits *within* an existing agent's files don't bump the dir mtime;
/// those still need a restart, which matches the low-churn A2A use case.
static ACP_AGENT_REGISTRY: OnceLock<
    Mutex<HashMap<PathBuf, (Option<std::time::SystemTime>, Arc<RwLock<AgentRegistry>>)>>,
> = OnceLock::new();

/// Get (or lazily build / refresh) the agent registry for `home_dir`.
async fn shared_agent_registry(home_dir: &Path) -> Result<Arc<RwLock<AgentRegistry>>, String> {
    let agents_dir = home_dir.join("agents");
    let current_mtime = std::fs::metadata(&agents_dir)
        .and_then(|m| m.modified())
        .ok();

    let map = ACP_AGENT_REGISTRY.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = map.lock().await;
    if let Some((cached_mtime, reg)) = guard.get(home_dir) {
        // Reuse only when we could read a stable mtime that matches the cache;
        // an unreadable mtime falls through to a rebuild (fail-safe).
        if current_mtime.is_some() && *cached_mtime == current_mtime {
            return Ok(reg.clone());
        }
    }
    let mut reg = AgentRegistry::new(agents_dir);
    reg.scan()
        .await
        .map_err(|e| format!("failed to scan agents directory: {e}"))?;
    let arc = Arc::new(RwLock::new(reg));
    guard.insert(home_dir.to_path_buf(), (current_mtime, arc.clone()));
    Ok(arc)
}

/// Resolve an A2A `context_id` to a concrete, existing target agent (RFC-25 R3).
///
/// - `"default"` / `""` → the registry's `Main`-role agent (the team root).
/// - any other value → that agent iff it exists in the registry.
///
/// Returns `None` when the requested target can't be resolved, so the caller can
/// surface a clean A2A error instead of dispatching to a non-existent agent.
async fn resolve_target_agent(
    registry: &Arc<RwLock<AgentRegistry>>,
    context_id: &str,
) -> Option<String> {
    let reg = registry.read().await;
    if context_id.is_empty() || context_id == "default" {
        return reg.main_agent().map(|a| a.config.agent.name.clone());
    }
    reg.get(context_id).map(|a| a.config.agent.name.clone())
}

/// Execute an A2A/ACP prompt by routing to the target agent through the
/// gateway's provider-agnostic dispatch (RFC-25 Phase 2 choke-point).
///
/// `agent_id` is the A2A `context_id` (the target agent; `"default"` → main agent).
/// The target agent's `[runtime] provider` decides the backend.
pub async fn handle_prompt_with_agent(
    home_dir: &Path,
    agent_id: &str,
    session_id: &str,
    message: &str,
    _model: Option<&str>,
) -> Vec<SessionUpdate> {
    info!(
        session_id,
        agent_id,
        message_len = message.len(),
        "ACP prompt → real agent execution (RFC-25 Phase 3)"
    );

    // Shared (cached) registry snapshot — no per-call agents-dir rescan (R3).
    let registry = match shared_agent_registry(home_dir).await {
        Ok(r) => r,
        Err(e) => {
            return vec![SessionUpdate::Error {
                session_id: session_id.to_string(),
                message: format!("ACP: {e}"),
            }];
        }
    };

    // Resolve the A2A target to a concrete existing agent (R3): "default"/"" →
    // the Main-role agent; otherwise the named agent iff present. Reject unknown
    // targets with a clean error instead of dispatching into the void.
    let target_agent = match resolve_target_agent(&registry, agent_id).await {
        Some(name) => name,
        None => {
            return vec![SessionUpdate::Error {
                session_id: session_id.to_string(),
                message: format!("ACP: unknown target agent '{agent_id}' (no such agent, and no Main-role agent for 'default')"),
            }];
        }
    };

    // Execute through the gateway delegation path. The target agent's runtime
    // provider is honoured inside call_claude_for_agent_with_type.
    match duduclaw_gateway::claude_runner::call_claude_for_agent_with_type(
        home_dir,
        &registry,
        &target_agent,
        message,
        duduclaw_gateway::cost_telemetry::RequestType::Dispatch,
    )
    .await
    {
        Ok(reply) => vec![SessionUpdate::Complete {
            session_id: session_id.to_string(),
            final_message: reply,
        }],
        Err(e) => vec![SessionUpdate::Error {
            session_id: session_id.to_string(),
            message: format!("ACP agent execution failed: {e}"),
        }],
    }
}

/// A2A Task lifecycle states.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum A2ATaskState {
    Working,
    Completed,
    Failed,
    Canceled,
    InputRequired,
}

/// An A2A task instance.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct A2ATask {
    pub id: String,
    pub context_id: String,
    pub state: A2ATaskState,
    pub description: String,
    pub result: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

impl A2ATask {
    pub fn new(id: String, context_id: String, description: String) -> Self {
        let now = chrono::Utc::now();
        Self {
            id,
            context_id,
            state: A2ATaskState::Working,
            description,
            result: None,
            created_at: now,
            updated_at: now,
        }
    }

    pub fn complete(&mut self, result: String) {
        self.state = A2ATaskState::Completed;
        self.result = Some(result);
        self.updated_at = chrono::Utc::now();
    }

    pub fn fail(&mut self, error: String) {
        self.state = A2ATaskState::Failed;
        self.result = Some(error);
        self.updated_at = chrono::Utc::now();
    }

    pub fn cancel(&mut self) {
        self.state = A2ATaskState::Canceled;
        self.updated_at = chrono::Utc::now();
    }
}

/// Manage A2A tasks in memory.
pub struct A2ATaskManager {
    tasks: std::collections::HashMap<String, A2ATask>,
}

impl A2ATaskManager {
    pub fn new() -> Self {
        Self {
            tasks: std::collections::HashMap::new(),
        }
    }

    pub fn create_task(&mut self, context_id: &str, description: &str) -> &A2ATask {
        let id = format!("a2a_task_{}", chrono::Utc::now().timestamp_millis());
        let task = A2ATask::new(id.clone(), context_id.to_string(), description.to_string());
        self.tasks.insert(id.clone(), task);
        self.tasks.get(&id).unwrap()
    }

    pub fn get_task(&self, id: &str) -> Option<&A2ATask> {
        self.tasks.get(id)
    }

    pub fn complete_task(&mut self, id: &str, result: String) -> bool {
        if let Some(task) = self.tasks.get_mut(id) {
            task.complete(result);
            true
        } else {
            false
        }
    }

    /// Mark a task as failed with an error message (A2A `Failed` state).
    pub fn fail_task(&mut self, id: &str, error: String) -> bool {
        if let Some(task) = self.tasks.get_mut(id) {
            task.fail(error);
            true
        } else {
            false
        }
    }

    pub fn cancel_task(&mut self, id: &str) -> bool {
        if let Some(task) = self.tasks.get_mut(id) {
            task.cancel();
            true
        } else {
            false
        }
    }
}
