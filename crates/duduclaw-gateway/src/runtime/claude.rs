//! Claude CLI runtime — wraps the existing `claude_runner` as an `AgentRuntime`.

use std::path::PathBuf;

use async_trait::async_trait;
use tracing::info;

use super::{AgentRuntime, RuntimeContext, RuntimeResponse};

/// Runtime that delegates to the Claude Code SDK (`claude` CLI).
pub struct ClaudeRuntime {
    home_dir: PathBuf,
}

impl ClaudeRuntime {
    pub fn new(home_dir: PathBuf) -> Self {
        Self { home_dir }
    }
}

#[async_trait]
impl AgentRuntime for ClaudeRuntime {
    fn name(&self) -> &str {
        "claude"
    }

    async fn execute(
        &self,
        prompt: &str,
        context: &RuntimeContext,
    ) -> Result<RuntimeResponse, String> {
        // Delegate to the existing account-rotated Claude CLI path.
        info!(agent = %context.agent_id, model = %context.model, "ClaudeRuntime: executing via claude CLI");

        let result = crate::channel_reply::call_claude_cli_rotated(
            prompt,
            &context.model,
            &context.system_prompt,
            &self.home_dir,
            context.agent_dir.as_deref(), // work_dir
            None,                         // on_progress
            None,                         // capabilities
            None,                         // session_id (history folded into prompt)
            &[],                          // conversation_history (threaded by caller for now)
        )
        .await?;

        Ok(RuntimeResponse {
            content: result,
            input_tokens: 0,  // Token counting happens at the telemetry layer
            output_tokens: 0,
            cache_read_tokens: 0,
            model_used: context.model.clone(),
            runtime_name: "claude".to_string(),
        })
    }

    async fn is_available(&self) -> bool {
        // Claude CLI is always assumed available (core requirement)
        true
    }
}
