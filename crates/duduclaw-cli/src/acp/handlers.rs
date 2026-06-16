//! ACP session handlers — maps ACP/A2A protocol operations to the DuDuClaw
//! agent runner.
//!
//! RFC-25 Phase 3: `handle_prompt_with_agent` now executes a REAL agent through
//! the gateway's provider-aware delegation path (`call_claude_for_agent_with_type`),
//! so an A2A `tasks/send` runs the target agent on its configured `[runtime]`
//! provider (Claude / Codex / Gemini / OpenAI-compat) — not a placeholder.

use std::path::Path;
use std::sync::Arc;

use tokio::sync::RwLock;
use tracing::info;

use duduclaw_agent::registry::AgentRegistry;

use super::types::*;

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

    // Build a registry snapshot from the home agents directory.
    let mut reg = AgentRegistry::new(home_dir.join("agents"));
    if let Err(e) = reg.scan().await {
        return vec![SessionUpdate::Error {
            session_id: session_id.to_string(),
            message: format!("ACP: failed to scan agents directory: {e}"),
        }];
    }
    let registry = Arc::new(RwLock::new(reg));

    // Execute through the gateway delegation path. The target agent's runtime
    // provider is honoured inside call_claude_for_agent_with_type.
    match duduclaw_gateway::claude_runner::call_claude_for_agent_with_type(
        home_dir,
        &registry,
        agent_id,
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
