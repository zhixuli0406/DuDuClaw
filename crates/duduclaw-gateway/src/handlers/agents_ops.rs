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

    pub(crate) async fn handle_agents_pause(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let agent_id = params
            .get("agent_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        info!(agent_id, "agents.pause requested");

        if let Err(e) = self.update_agent_status_as(agent_id, "paused", "agents.pause", ctx).await {
            return WsFrame::error_response("", &format!("Failed to pause agent: {e}"));
        }

        WsFrame::ok_response(
            "",
            json!({ "success": true, "name": agent_id, "status": "paused" }),
        )
    }

    pub(crate) async fn handle_agents_resume(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let agent_id = params
            .get("agent_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        info!(agent_id, "agents.resume requested");

        if let Err(e) = self.update_agent_status_as(agent_id, "active", "agents.resume", ctx).await {
            return WsFrame::error_response("", &format!("Failed to resume agent: {e}"));
        }

        WsFrame::ok_response(
            "",
            json!({ "success": true, "name": agent_id, "status": "active" }),
        )
    }

    /// Read-modify-write an agent's `agent.toml` using the provided mutation closure.
    ///
    /// The mutation runs on the file as read under the cross-process lock and
    /// is written atomically through the profile file writer (version,
    /// audit). After a successful write the registry is re-scanned, with
    /// retries.
    ///
    /// Returns `Ok(true)` when the re-scan succeeded (changes visible
    /// immediately) and `Ok(false)` when it failed three times: the change is
    /// saved, but the gateway has no periodic re-scan, so it takes effect
    /// after the next successful scan (any later edit, or a restart).
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

    /// [`Self::update_agent_toml`] for a dashboard caller: the version and
    /// audit row name them, and when `session` is given their access is
    /// re-read from `users.db` inside the file lock (a binding revoked while
    /// the request was in flight stops the write).
    pub(crate) async fn update_agent_toml_as<F>(
        &self,
        agent_id: &str,
        source: &'static str,
        caller: Option<&UserContext>,
        session: Option<&UserContext>,
        mutate: F,
    ) -> Result<bool, String>
    where
        F: FnOnce(&mut toml::Table) -> Result<(), String>,
    {
        let home = self.home_dir.clone();
        let agent = agent_id.to_string();
        let session = session.cloned();
        let guard = move || match &session {
            Some(s) => crate::agent_files::flow::require_live_agent_access(&home, s, &agent, AccessLevel::Owner).map(|_| ()),
            None => Ok(()),
        };
        let edit = crate::channel_reply::AgentTomlEdit {
            source,
            user_id: caller.map(|c| c.user_id.as_str()).unwrap_or("system"),
            guard,
        };
        crate::channel_reply::update_agent_toml_edit(&self.registry, agent_id, edit, mutate).await
    }

    /// Update status with the caller's role and binding re-checked inside the file lock.
    pub(crate) async fn update_agent_status_as(
        &self,
        agent_id: &str,
        status: &str,
        source: &'static str,
        ctx: &UserContext,
    ) -> Result<(), String> {
        let status = status.to_string();
        let edit = crate::channel_reply::AgentTomlEdit {
            source,
            user_id: &ctx.user_id,
            guard: || {
                let live = crate::agent_files::flow::require_live_agent_access(
                    &self.home_dir, ctx, agent_id, AccessLevel::Operator,
                )?;
                acl::require_role(&live, UserRole::Manager)
            },
        };
        crate::channel_reply::update_agent_toml_edit(&self.registry, agent_id, edit, move |table| {
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
