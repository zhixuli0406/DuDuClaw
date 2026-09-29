//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    /// Dashboard-initiated delegation.  Supervisor pattern is NOT enforced here
    /// because this RPC is an operator-level action (depth always starts at 0).
    /// Agent-to-agent delegation goes through MCP `send_to_agent` which IS enforced.
    pub(crate) async fn handle_agents_delegate(&self, params: Value) -> WsFrame {
        let agent_id = params
            .get("agent_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let prompt = params.get("prompt").and_then(|v| v.as_str()).unwrap_or("");
        let wait = params
            .get("wait_for_response")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        // Enforce prompt length limit to prevent abuse (MCP-H1)
        const MAX_PROMPT_LEN: usize = 100_000;
        if prompt.len() > MAX_PROMPT_LEN {
            return WsFrame::error_response(
                "",
                &format!(
                    "Prompt too long: {} chars (max {MAX_PROMPT_LEN})",
                    prompt.len()
                ),
            );
        }

        info!(agent_id, "agents.delegate requested (dashboard)");

        // Verify target agent exists
        let reg = self.registry.read().await;
        let agent = match reg.get(agent_id) {
            Some(a) => a.clone(),
            None => return WsFrame::error_response("", &format!("Agent not found: {agent_id}")),
        };
        // F2: an archived / soft-deleted agent must not receive delegated work.
        // Fail-closed with an explicit error rather than silently queueing a
        // task for an off-boarded agent.
        if !agent.config.agent.status.is_operational() {
            let status = format!("{:?}", agent.config.agent.status).to_lowercase();
            return WsFrame::error_response(
                "",
                &format!(
                    "Agent '{agent_id}' is not operational (status: {status}); cannot delegate."
                ),
            );
        }
        let model = agent.config.model.preferred.clone();
        drop(reg);

        let message_id = uuid::Uuid::new_v4().to_string();

        if wait {
            // Synchronous delegation: Rust-native Direct API call (no Python).
            let home = self.home_dir.clone();
            let system_prompt = agent
                .soul
                .as_deref()
                .unwrap_or("You are a helpful AI agent.")
                .to_string();
            match crate::channel_reply::call_direct_api_delegate(
                prompt,
                &model,
                &system_prompt,
                &home,
            )
            .await
            {
                Ok(response) => WsFrame::ok_response(
                    "",
                    json!({
                        "success": true,
                        "message_id": message_id,
                        "target_agent": agent_id,
                        "response": response,
                        "status": "completed",
                    }),
                ),
                Err(e) => WsFrame::error_response("", &format!("Delegate execution failed: {e}")),
            }
        } else {
            // Async delegation: write to bus queue for background processing
            let queue_path = self.home_dir.join("bus_queue.jsonl");
            let task = serde_json::json!({
                "type": "agent_message",
                "message_id": &message_id,
                "agent_id": agent_id,
                "payload": prompt,
                "timestamp": chrono::Utc::now().to_rfc3339(),
                "delegation_depth": 0,
                "origin_agent": "dashboard",
                "sender_agent": "dashboard",
            });
            let task_str = task.to_string();
            if let Err(e) = crate::dispatcher::append_line(&queue_path, &task_str).await {
                return WsFrame::error_response("", &format!("Failed to queue delegation: {e}"));
            }

            WsFrame::ok_response(
                "",
                json!({
                    "success": true,
                    "message_id": message_id,
                    "target_agent": agent_id,
                    "status": "queued",
                }),
            )
        }
    }

    pub(crate) async fn handle_agents_pause(&self, params: Value) -> WsFrame {
        let agent_id = params
            .get("agent_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        info!(agent_id, "agents.pause requested");

        if let Err(e) = self.update_agent_status(agent_id, "paused").await {
            return WsFrame::error_response("", &format!("Failed to pause agent: {e}"));
        }

        WsFrame::ok_response(
            "",
            json!({ "success": true, "name": agent_id, "status": "paused" }),
        )
    }

    pub(crate) async fn handle_agents_resume(&self, params: Value) -> WsFrame {
        let agent_id = params
            .get("agent_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        info!(agent_id, "agents.resume requested");

        if let Err(e) = self.update_agent_status(agent_id, "active").await {
            return WsFrame::error_response("", &format!("Failed to resume agent: {e}"));
        }

        WsFrame::ok_response(
            "",
            json!({ "success": true, "name": agent_id, "status": "active" }),
        )
    }

    /// Read-modify-write an agent's `agent.toml` using the provided mutation closure.
    ///
    /// Uses atomic write (temp + rename) to prevent corruption on concurrent access.
    /// After a successful write, attempts to trigger a registry re-scan for hot-reload.
    ///
    /// Returns `Ok(true)` if the registry was re-scanned in time (changes visible
    /// immediately), `Ok(false)` if the re-scan was skipped due to lock contention
    /// or scan error (changes will land on the next periodic sync, ≤ 5 min for
    /// heartbeat-driven consumers; channel_reply / dispatcher always read fresh
    /// from the lock-protected registry so they see the previous version until the
    /// next scan).
    /// Thin wrapper over [`crate::channel_reply::update_agent_toml_with`] — the
    /// single implementation of "edit agent.toml + hot-reload", shared with the
    /// chat-command path (`/model`) so the two can never drift on the atomic
    /// write or the registry rescan.
    pub(crate) async fn update_agent_toml<F>(&self, agent_id: &str, mutate: F) -> Result<bool, String>
    where
        F: FnOnce(&mut toml::Table) -> Result<(), String>,
    {
        crate::channel_reply::update_agent_toml_with(&self.registry, agent_id, mutate).await
    }

    /// Convenience: update only the `status` field in an agent's `agent.toml`.
    pub(crate) async fn update_agent_status(&self, agent_id: &str, status: &str) -> Result<(), String> {
        let status = status.to_string();
        self.update_agent_toml(agent_id, move |table| {
            let agent_section = table
                .get_mut("agent")
                .and_then(|v| v.as_table_mut())
                .ok_or_else(|| "agent.toml missing [agent] section".to_string())?;
            agent_section.insert("status".to_string(), toml::Value::String(status.clone()));
            info!("Agent status updated to {status}");
            Ok(())
        })
        .await?;
        Ok(())
    }

    /// Demote the current main agent to "specialist", skipping `except_id`.
    /// This ensures at most one agent has the "main" role at any time.
    pub(crate) async fn demote_current_main(&self, except_id: &str) -> Result<(), String> {
        let current_main = {
            let reg = self.registry.read().await;
            reg.main_agent()
                .filter(|a| a.config.agent.name != except_id)
                .map(|a| a.config.agent.name.clone())
        };
        if let Some(old_main) = current_main {
            info!(
                old_main = old_main.as_str(),
                "Demoting current main agent to specialist"
            );
            self.update_agent_toml(&old_main, |table| {
                let agent_section = table
                    .get_mut("agent")
                    .and_then(|v| v.as_table_mut())
                    .ok_or_else(|| "agent.toml missing [agent] section".to_string())?;
                agent_section.insert("role".into(), toml::Value::String("specialist".into()));
                Ok(())
            })
            .await?;
        }
        Ok(())
    }
}
