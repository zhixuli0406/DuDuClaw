//! ACP session handlers — maps ACP protocol operations to DuDuClaw agent runner.
//!
//! Future work: wire to AgentRunner for real agent execution.

use duduclaw_core::truncate_bytes;
use tracing::info;

use super::types::*;

/// Handle a session prompt by routing to the appropriate agent.
///
/// Currently returns a placeholder response. Full implementation will:
/// 1. Resolve agent from session config
/// 2. Build system prompt via SystemPromptSnapshot
/// 3. Call Claude CLI or Direct API
/// 4. Stream SessionUpdate notifications back
pub fn handle_prompt_with_agent(
    session_id: &str,
    message: &str,
    model: Option<&str>,
) -> Vec<SessionUpdate> {
    info!(session_id, message_len = message.len(), model, "ACP prompt received");

    let mut updates = Vec::new();

    // Simulate a thinking step
    updates.push(SessionUpdate::TextChunk {
        session_id: session_id.to_string(),
        content: format!("Processing: {}", truncate_bytes(message, 50)),
    });

    // Final response
    updates.push(SessionUpdate::Complete {
        session_id: session_id.to_string(),
        final_message: format!(
            "ACP response for session {}. Model: {}. Agent runner integration pending.",
            session_id,
            model.unwrap_or("default")
        ),
    });

    updates
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

    pub fn cancel_task(&mut self, id: &str) -> bool {
        if let Some(task) = self.tasks.get_mut(id) {
            task.cancel();
            true
        } else {
            false
        }
    }
}
