//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    /// Dashboard-initiated delegation.  Supervisor pattern is NOT enforced here
    /// because this RPC is an operator-level action (depth always starts at 0).
    /// Agent-to-agent delegation goes through MCP `send_to_agent` which IS enforced.
    pub(crate) async fn handle_agents_delegate(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let agent_id = params
            .get("agent_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        // v1.81 P1d: delegation (its answer runs under the employee's SOUL):
        // canonical id, owner-only for a Personal employee, no break-glass.
        if let Err(frame) = self.delegation_check(ctx, agent_id).await {
            return frame;
        }
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

        if let Err(frame) = self.deny_main_for_non_admin(ctx, agent_id).await { return frame; }

        let message_id = uuid::Uuid::new_v4().to_string();

        if wait {
            if let Err(refusal) = crate::budget::gate_before_spawn(&self.home_dir, Some(&agent.dir), agent_id, "delegate_sync").await {
                return WsFrame::error_response("", &refusal.to_string());
            }
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
        if let Err(frame) = self.deny_main_for_non_admin(ctx, agent_id).await { return frame; }

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
        self.update_agent_toml_as_within(agent_id, source, caller, session, None, mutate).await
    }

    /// [`Self::update_agent_toml_as`] with an optional bound on the
    /// `agent.toml` lock wait (RM-027 round 5: `Some` while the caller holds
    /// the global org lock — past it nothing is written).
    pub(crate) async fn update_agent_toml_as_within<F>(
        &self,
        agent_id: &str,
        source: &'static str,
        caller: Option<&UserContext>,
        session: Option<&UserContext>,
        lock_wait: Option<std::time::Duration>,
        mutate: F,
    ) -> Result<bool, String>
    where
        F: FnOnce(&mut toml::Table) -> Result<(), String>,
    {
        let home = self.home_dir.clone();
        let agent = agent_id.to_string();
        let session = session.cloned();
        let caller_for_guard = caller.cloned();
        let guard = move || {
            let live = match &session {
                Some(s) => Some(crate::agent_files::flow::require_live_agent_access(
                    &home, s, &agent, AccessLevel::Owner,
                )?),
                None if source == "agents.update" => caller_for_guard.as_ref()
                    .map(|ctx| live_reader_context(&home, ctx)
                        .map_err(|_| "permission denied".to_owned()))
                    .transpose()?,
                None => None,
            };
            if source == "agents.update" {
                require_non_main_commit_target(&home, live.as_ref(), &agent)?;
            }
            Ok(())
        };
        let edit = crate::channel_reply::AgentTomlEdit {
            source,
            user_id: caller.map(|c| c.user_id.as_str()).unwrap_or("system"),
            guard,
        };
        #[cfg(test)]
        main_commit_test_seam::reached(&self.home_dir);
        match lock_wait {
            None => crate::channel_reply::update_agent_toml_edit(&self.registry, agent_id, edit, mutate).await,
            Some(wait) => {
                crate::channel_reply::update_agent_toml_edit_bounded(&self.registry, agent_id, edit, wait, mutate).await
            }
        }
    }

    /// Update status with the caller's role and binding re-checked inside the file lock.
    pub(crate) async fn update_agent_status_as(
        &self,
        agent_id: &str,
        status: &str,
        source: &'static str,
        ctx: &UserContext,
    ) -> Result<(), String> {
        let event_status = status.to_string();
        let status = status.to_string();
        let edit = crate::channel_reply::AgentTomlEdit {
            source,
            user_id: &ctx.user_id,
            guard: || {
                let live = crate::agent_files::flow::require_live_agent_access(
                    &self.home_dir, ctx, agent_id, AccessLevel::Operator,
                )?;
                acl::require_role(&live, UserRole::Manager)?;
                if source == "agents.resume" {
                    require_non_main_commit_target(&self.home_dir, Some(&live), agent_id)?;
                }
                Ok(())
            },
        };
        #[cfg(test)]
        main_commit_test_seam::reached(&self.home_dir);
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
        self.broadcast_event("agent.status_changed",
            json!({ "agent_id": agent_id, "status": event_status })).await;
        Ok(())
    }

    /// Demote the current main agent to "specialist", skipping `except_id`.
    /// This ensures at most one agent has the "main" role at any time.
    ///
    /// Returns the [`MainDemotion`] it made (`None` when there was no other
    /// main). RM-027 round 5: a caller that demotes *before* a step that can
    /// still fail (agent creation: the authority commit, the cycle check, the
    /// directory publish) must hand it to [`Self::restore_demoted_main`] on
    /// every failure, so the old main never stays `specialist` for a create
    /// that did not happen. The `agent.toml` lock wait is bounded by
    /// [`crate::org_change_approval::MIRROR_LOCK_WAIT`] (creation and, since
    /// round 6, an `agents.update` promotion call this under the global org
    /// lock, and restore on failure).
    pub(crate) async fn demote_current_main(&self, except_id: &str) -> Result<Option<MainDemotion>, String> {
        let current_main = {
            let reg = self.registry.read().await;
            reg.main_agent()
                .filter(|a| a.config.agent.name != except_id)
                .map(|a| a.config.agent.name.clone())
        };
        let Some(old_main) = current_main else { return Ok(None) };
        info!(
            old_main = old_main.as_str(),
            "Demoting current main agent to specialist"
        );
        let prior_role = std::sync::Arc::new(std::sync::Mutex::new(None::<toml::Value>));
        let seen = prior_role.clone();
        self.set_main_role_bounded(&old_main, move |agent_section| {
            if let Ok(mut s) = seen.lock() {
                *s = agent_section.get("role").cloned();
            }
            agent_section.insert("role".into(), toml::Value::String("specialist".into()));
        })
        .await?;
        let prior_role = prior_role.lock().ok().and_then(|mut s| s.take());
        Ok(Some(MainDemotion { old_main, prior_role }))
    }

    /// Undo a [`MainDemotion`] after the step it preceded failed. Only our own
    /// demotion is undone: the role is put back only while it still reads
    /// `specialist` (a role somebody changed since is left alone). Retried
    /// (transient lock / IO); a restore that still fails is audited
    /// (`agent_main_demotion_restore_failed`) and returned.
    pub(crate) async fn restore_demoted_main(&self, demotion: MainDemotion) -> Result<(), String> {
        let MainDemotion { old_main, prior_role } = demotion;
        let prior = prior_role.unwrap_or_else(|| toml::Value::String("main".into()));
        let mut last = String::new();
        for attempt in 1..=3u32 {
            let prior = prior.clone();
            let result = self
                .set_main_role_bounded(&old_main, move |agent_section| {
                    if agent_section.get("role").and_then(|v| v.as_str()) == Some("specialist") {
                        agent_section.insert("role".into(), prior);
                    }
                })
                .await;
            match result {
                Ok(()) => {
                    info!(old_main = old_main.as_str(), "Restored the previous main agent after a failed create / promotion");
                    return Ok(());
                }
                Err(e) => {
                    warn!(old_main = old_main.as_str(), attempt, error = %e, "restoring the previous main agent failed");
                    last = e;
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                }
            }
        }
        crate::security_autopilot::audit_and_emit(
            &self.home_dir,
            &duduclaw_security::audit::AuditEvent::new(
                "agent_main_demotion_restore_failed",
                &old_main,
                duduclaw_security::audit::Severity::Warning,
                json!({ "agent_id": old_main, "error": duduclaw_core::truncate_chars(&last, 300) }),
            ),
        );
        Err(last)
    }

    /// One `[agent] role` edit of `agent_id` through the profile writer (the
    /// same system source `update_agent_toml` records), with the
    /// `agent.toml` lock wait bounded.
    async fn set_main_role_bounded(
        &self,
        agent_id: &str,
        edit_role: impl FnOnce(&mut toml::Table),
    ) -> Result<(), String> {
        let edit = crate::channel_reply::AgentTomlEdit { source: "agents.update", user_id: "system", guard: || Ok(()) };
        let wait = crate::org_change_approval::mirror_lock_wait(&self.home_dir);
        crate::channel_reply::update_agent_toml_edit_bounded(&self.registry, agent_id, edit, wait, move |table| {
            let agent_section = table
                .get_mut("agent")
                .and_then(|v| v.as_table_mut())
                .ok_or_else(|| "agent.toml missing [agent] section".to_string())?;
            edit_role(agent_section);
            Ok(())
        })
        .await
        .map(|_| ())
    }
}

/// RM-027 round 5: a demotion [`MethodHandler::demote_current_main`] made,
/// kept so a failed create can put the old main back
/// ([`MethodHandler::restore_demoted_main`]).
#[derive(Debug)]
#[must_use = "a demotion made before a failing step must be restored"]
pub(crate) struct MainDemotion {
    old_main: String,
    /// The `[agent] role` value before the demotion (`None` = absent).
    prior_role: Option<toml::Value>,
}

/// Called only by the writer's commit guard while agent.toml is locked, after
/// any lifecycle drain. Registry snapshots and the request's early check may
/// predate a promotion; use the live caller and the still-unmodified file.
fn require_non_main_commit_target(
    home: &Path,
    live: Option<&UserContext>,
    agent_id: &str,
) -> Result<(), String> {
    if live.is_some_and(UserContext::is_admin) { return Ok(()); }
    let refuse = || "主代理僅管理員可以使用。".to_owned();
    let raw = std::fs::read_to_string(home.join("agents").join(agent_id).join("agent.toml"))
        .map_err(|_| refuse())?;
    // Only authorization fields belong in this gate. Unrelated optional or
    // repairable configuration must not turn an ordinary target into Main.
    #[derive(serde::Deserialize)]
    struct LockedTarget { agent: LockedIdentity }
    #[derive(serde::Deserialize)]
    struct LockedIdentity { name: String, role: duduclaw_core::types::AgentRole }
    let agent: LockedTarget = toml::from_str(&raw).map_err(|_| refuse())?;
    let raw = match std::fs::read_to_string(home.join("config.toml")) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(_) => return Err(refuse()),
    };
    let config: toml::Value = raw.parse().map_err(|_| refuse())?;
    let default = match config.get("general") {
        None => None,
        Some(general) => match general.as_table().ok_or_else(refuse)?.get("default_agent") {
            None => None,
            Some(value) => Some(value.as_str().ok_or_else(refuse)?),
        },
    };
    if agent.agent.role == duduclaw_core::types::AgentRole::Main
        || default.is_some_and(|id| id == agent_id || id == agent.agent.name)
    {
        return Err(refuse());
    }
    Ok(())
}

#[cfg(test)]
pub(super) mod main_commit_test_seam {
    use std::{collections::HashMap, path::{Path, PathBuf}, sync::{Mutex, OnceLock}};
    use tokio::sync::oneshot;

    fn waiters() -> &'static Mutex<HashMap<PathBuf, oneshot::Sender<()>>> {
        static WAITERS: OnceLock<Mutex<HashMap<PathBuf, oneshot::Sender<()>>>> = OnceLock::new();
        WAITERS.get_or_init(|| Mutex::new(HashMap::new()))
    }

    pub(crate) fn arm(home: &Path) -> oneshot::Receiver<()> {
        let (tx, rx) = oneshot::channel();
        assert!(waiters().lock().unwrap().insert(home.to_path_buf(), tx).is_none());
        rx
    }

    pub(super) fn reached(home: &Path) {
        if let Some(tx) = waiters().lock().unwrap().remove(home) {
            let _ = tx.send(());
        }
    }
}
