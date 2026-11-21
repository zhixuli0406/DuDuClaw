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
        if crate::runtime_dispatch::in_utility_call() {
            let content = crate::channel_reply::call_claude_cli_public(
                prompt, &context.model, &context.system_prompt, &self.home_dir,
            ).await?;
            return Ok(RuntimeResponse {
                content, model_used: context.model.clone(), runtime_name: "claude".into(),
                ..Default::default()
            });
        }
        // Delegate to the existing account-rotated Claude CLI path.
        info!(agent = %context.agent_id, model = %context.model, "ClaudeRuntime: executing via claude CLI");

        // RM-203: the dispatch choke-point (and an explicit runtime fallback,
        // which clones the primary's context) carries the employee's capped
        // conversation here. The callee folds it into the prompt exactly as
        // the channel-reply Claude path does; callers with no conversation
        // (delegation, evolution) already pass an empty history.
        let conversation_history = context.conversation_history.as_slice();
        #[cfg(test)]
        if let Ok(capture) = tests::HISTORY_CAPTURE.try_with(std::sync::Arc::clone) {
            *capture.lock().unwrap() = Some(conversation_history.to_vec());
            return Err("claude runtime arguments captured before spawn".into());
        }

        let result = crate::channel_reply::call_claude_cli_rotated(
            prompt,
            &context.model,
            &context.system_prompt,
            &self.home_dir,
            context.agent_dir.as_deref(), // work_dir
            None,                         // on_progress
            // W1: forward the agent's capability restrictions so the Claude CLI
            // spawn applies --allowedTools / --disallowedTools (previously
            // dropped on the dispatch path — a silent enforcement gap).
            context.capabilities.as_ref(),
            None, // session_id (history folded into prompt)
            conversation_history,
            // G1: agent's `[model] account_pool` — narrows the rotator
            // candidate set (fail-open; empty ⇒ full set).
            &context.account_pool,
            // P1/WP-3: the multi-runtime choke-point's resolved effort (a team
            // role spec, else the agent's `agent.toml [model] effort`). This is
            // how effort reaches Claude through `run_agent_prompt`; `None` here
            // still falls back to the agent dir inside the callee.
            context.effort,
            // Utility calls took the lightweight lane above; everything else
            // is an employee's work and needs its directory (fail closed).
            crate::runtime_dispatch::SpawnPurpose::EmployeeTurn,
        )
        .await?;

        Ok(RuntimeResponse {
            partial_timeout_secs: None,
            content: result,
            input_tokens: 0, // Token counting happens at the telemetry layer
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::ConversationTurn;
    use std::sync::{Arc, Mutex};

    tokio::task_local! {
        /// Captures the conversation `execute` would hand the CLI spawn and
        /// stops before any account lookup or process start.
        pub(super) static HISTORY_CAPTURE: Arc<Mutex<Option<Vec<ConversationTurn>>>>;
    }

    fn turn(role: &str, content: &str) -> ConversationTurn {
        ConversationTurn { role: role.into(), content: content.into() }
    }

    fn pairs(turns: &[ConversationTurn]) -> Vec<(String, String)> {
        turns.iter().map(|t| (t.role.clone(), t.content.clone())).collect()
    }

    #[tokio::test]
    async fn employee_turn_forwards_the_context_conversation_history() {
        let home = tempfile::tempdir().unwrap();
        let history = vec![turn("user", "第一輪：請記住代號是藍鯨"), turn("assistant", "好的，代號是藍鯨。")];
        let context = RuntimeContext {
            agent_dir: None,
            system_prompt: String::new(),
            model: "claude-sonnet-4-6".into(),
            max_tokens: 1024,
            home_dir: home.path().to_path_buf(),
            agent_id: "worker".into(),
            preferred_provider: None,
            conversation_history: history.clone(),
            capabilities: None,
            account_pool: vec![],
            effort: None,
            allow_cross_family_failover: true,
        };
        let captured = Arc::new(Mutex::new(None));
        let error = HISTORY_CAPTURE
            .scope(captured.clone(), ClaudeRuntime::new(home.path().to_path_buf()).execute("代號是什麼？", &context))
            .await
            .unwrap_err();
        assert_eq!(error, "claude runtime arguments captured before spawn");
        let seen = captured.lock().unwrap().take().expect("history captured");
        assert_eq!(pairs(&seen), pairs(&history));
    }
}
