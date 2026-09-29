//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    /// Soft-delete an agent (WP4). Marks `status = "deleted"` and halts the
    /// heartbeat/evolution kill-switch. **No data is removed** — the agent
    /// directory and its `memory.db` stay on disk; the agent simply disappears
    /// from every list/route. Refuses the main agent (name kept `remove` for
    /// front-end compatibility, but the semantics are now soft, not hard).
    pub(crate) async fn handle_agents_remove(&self, params: Value) -> WsFrame {
        let agent_id = match params.get("agent_id").and_then(|v| v.as_str()) {
            Some(id) if !id.is_empty() => id.to_string(),
            _ => return WsFrame::error_response("", "Missing 'agent_id' parameter"),
        };

        if !is_valid_agent_id(&agent_id) {
            return WsFrame::error_response("", "Invalid agent_id format");
        }

        // Refuse to remove the main / default agent — it anchors routing.
        {
            let reg = self.registry.read().await;
            match reg.get(&agent_id) {
                Some(agent) => {
                    if matches!(
                        agent.config.agent.role,
                        duduclaw_core::types::AgentRole::Main
                    ) {
                        return WsFrame::error_response("", "Cannot remove the main agent");
                    }
                }
                None => {
                    return WsFrame::error_response("", &format!("Agent not found: {agent_id}"));
                }
            }
        }

        if let Err(e) = self.offboard_agent_toml(&agent_id, "deleted").await {
            return WsFrame::error_response("", &format!("Failed to soft-delete agent: {e}"));
        }

        crate::security_autopilot::audit_and_emit(
            &self.home_dir,
            &duduclaw_security::audit::AuditEvent::new(
                "agent_soft_delete",
                &agent_id,
                duduclaw_security::audit::Severity::Warning,
                json!({ "status": "deleted", "data_retained": true, "source": "dashboard" }),
            ),
        );

        info!(
            agent_id = agent_id.as_str(),
            "Agent soft-deleted (data retained on disk)"
        );
        WsFrame::ok_response(
            "",
            json!({
                "success": true,
                "agent_id": agent_id,
                "status": "deleted",
                "data_retained": true,
            }),
        )
    }

    /// Set `status` **and** flip the freeze kill-switch (`heartbeat.enabled` /
    /// `evolution.enabled = false`) in one atomic `agent.toml` write. Shared by
    /// archive + soft-delete so an off-boarded agent never keeps ticking.
    pub(crate) async fn offboard_agent_toml(&self, agent_id: &str, status: &str) -> Result<(), String> {
        let status = status.to_string();
        self.update_agent_toml(agent_id, move |table| offboard_freeze_table(table, &status))
            .await?;
        Ok(())
    }

    /// Archive an agent (WP4): recoverable off-board. `status = "archived"` +
    /// freeze kill-switch; hidden from the default roster but fully restorable
    /// via `agents.unarchive`. No data is touched.
    pub(crate) async fn handle_agents_archive(&self, params: Value) -> WsFrame {
        let agent_id = match params.get("agent_id").and_then(|v| v.as_str()) {
            Some(id) if !id.is_empty() => id.to_string(),
            _ => return WsFrame::error_response("", "Missing 'agent_id' parameter"),
        };
        if !is_valid_agent_id(&agent_id) {
            return WsFrame::error_response("", "Invalid agent_id format");
        }
        {
            let reg = self.registry.read().await;
            match reg.get(&agent_id) {
                Some(agent) => {
                    if matches!(
                        agent.config.agent.role,
                        duduclaw_core::types::AgentRole::Main
                    ) {
                        return WsFrame::error_response("", "Cannot archive the main agent");
                    }
                }
                None => {
                    return WsFrame::error_response("", &format!("Agent not found: {agent_id}"));
                }
            }
        }
        if let Err(e) = self.offboard_agent_toml(&agent_id, "archived").await {
            return WsFrame::error_response("", &format!("Failed to archive agent: {e}"));
        }
        crate::security_autopilot::audit_and_emit(
            &self.home_dir,
            &duduclaw_security::audit::AuditEvent::new(
                "agent_archive",
                &agent_id,
                duduclaw_security::audit::Severity::Warning,
                json!({ "status": "archived", "source": "dashboard" }),
            ),
        );
        info!(agent_id = agent_id.as_str(), "Agent archived (recoverable)");
        WsFrame::ok_response(
            "",
            json!({
                "success": true,
                "agent_id": agent_id,
                "status": "archived",
            }),
        )
    }

    /// Restore an archived (or soft-deleted) agent to `active` and re-enable the
    /// heartbeat/evolution kill-switch.
    pub(crate) async fn handle_agents_unarchive(&self, params: Value) -> WsFrame {
        let agent_id = match params.get("agent_id").and_then(|v| v.as_str()) {
            Some(id) if !id.is_empty() => id.to_string(),
            _ => return WsFrame::error_response("", "Missing 'agent_id' parameter"),
        };
        if !is_valid_agent_id(&agent_id) {
            return WsFrame::error_response("", "Invalid agent_id format");
        }
        {
            let reg = self.registry.read().await;
            if reg.get(&agent_id).is_none() {
                return WsFrame::error_response("", &format!("Agent not found: {agent_id}"));
            }
        }
        let res = self
            .update_agent_toml(&agent_id, unarchive_restore_table)
            .await;
        if let Err(e) = res {
            return WsFrame::error_response("", &format!("Failed to unarchive agent: {e}"));
        }
        crate::security_autopilot::audit_and_emit(
            &self.home_dir,
            &duduclaw_security::audit::AuditEvent::new(
                "agent_unarchive",
                &agent_id,
                duduclaw_security::audit::Severity::Info,
                json!({ "status": "active", "source": "dashboard" }),
            ),
        );
        info!(
            agent_id = agent_id.as_str(),
            "Agent unarchived (restored to active)"
        );
        WsFrame::ok_response(
            "",
            json!({
                "success": true,
                "agent_id": agent_id,
                "status": "active",
            }),
        )
    }

    /// Hand off an off-boarding agent's work to a successor (WP4). Moves any
    /// combination of memory / wiki / open tasks (each a boolean switch, default
    /// on) from `from_agent` to `to_agent`, then archives the source unless
    /// `auto_archive = false`. Every sub-move reports its own count; a failure in
    /// one sub-move is surfaced as `status: "PARTIAL"` (never silently swallowed).
    pub(crate) async fn handle_agents_handoff(&self, params: Value) -> WsFrame {
        let from_agent = match params.get("from_agent").and_then(|v| v.as_str()) {
            Some(id) if !id.is_empty() => id.to_string(),
            _ => return WsFrame::error_response("", "Missing 'from_agent' parameter"),
        };
        let to_agent = match params.get("to_agent").and_then(|v| v.as_str()) {
            Some(id) if !id.is_empty() => id.to_string(),
            _ => return WsFrame::error_response("", "Missing 'to_agent' parameter"),
        };
        if !is_valid_agent_id(&from_agent) || !is_valid_agent_id(&to_agent) {
            return WsFrame::error_response("", "Invalid agent_id format");
        }
        if from_agent == to_agent {
            return WsFrame::error_response("", "from_agent and to_agent must differ");
        }
        {
            let reg = self.registry.read().await;
            if reg.get(&from_agent).is_none() {
                return WsFrame::error_response("", &format!("Agent not found: {from_agent}"));
            }
            if reg.get(&to_agent).is_none() {
                return WsFrame::error_response("", &format!("Agent not found: {to_agent}"));
            }
        }

        let do_memory = params
            .get("memory")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let do_wiki = params.get("wiki").and_then(|v| v.as_bool()).unwrap_or(true);
        let do_tasks = params
            .get("tasks")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let auto_archive = params
            .get("auto_archive")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);

        let mut result = json!({
            "success": true,
            "from_agent": from_agent,
            "to_agent": to_agent,
        });
        let mut errors: Vec<String> = Vec::new();

        if do_memory {
            match self.handoff_memory(&from_agent, &to_agent).await {
                Ok(summary) => {
                    result["memory"] = json!({
                        "moved": summary.total(),
                        "memories": summary.memories,
                        "key_facts": summary.key_facts,
                        "archived_rows": summary.archived,
                    });
                }
                Err(e) => {
                    result["memory"] = json!({ "error": e });
                    errors.push(format!("memory: {e}"));
                }
            }
        }

        if do_wiki {
            match self.handoff_wiki(&from_agent, &to_agent).await {
                Ok(count) => result["wiki"] = json!({ "files_moved": count }),
                Err(e) => {
                    result["wiki"] = json!({ "error": e });
                    errors.push(format!("wiki: {e}"));
                }
            }
        }

        if do_tasks {
            match self.task_store().await {
                Ok(store) => {
                    let now = chrono::Utc::now().to_rfc3339();
                    match store
                        .reassign_open_tasks(&from_agent, &to_agent, &now)
                        .await
                    {
                        Ok(n) => result["tasks"] = json!({ "reassigned": n }),
                        Err(e) => {
                            result["tasks"] = json!({ "error": e });
                            errors.push(format!("tasks: {e}"));
                        }
                    }
                }
                Err(_) => {
                    result["tasks"] = json!({ "error": "task store unavailable" });
                    errors.push("tasks: store unavailable".to_string());
                }
            }
        }

        if auto_archive && errors.is_empty() {
            if let Err(e) = self.offboard_agent_toml(&from_agent, "archived").await {
                result["auto_archive"] = json!({ "error": e });
                errors.push(format!("auto_archive: {e}"));
            } else {
                result["auto_archive"] = json!({ "archived": true });
            }
        } else if auto_archive {
            result["auto_archive"] = json!({ "skipped": "handoff had errors" });
        }

        result["status"] = json!(if errors.is_empty() {
            "COMPLETE"
        } else {
            "PARTIAL"
        });
        if !errors.is_empty() {
            result["success"] = json!(false);
            result["errors"] = json!(errors);

            // F7: a PARTIAL handoff is a split-brain — some data already moved to
            // `to_agent` while `from_agent` is still live and NOT archived
            // (auto_archive was skipped above). We cannot do an atomic
            // cross-subsystem transaction (memory / wiki / tasks are three
            // independent stores), so surface the split honestly: flag the
            // source agent as needing manual reconciliation and spell out which
            // subsystems moved vs. did not, so the operator can converge it by
            // hand rather than believing the handoff finished.
            let mut moved: Vec<&str> = Vec::new();
            let mut not_moved: Vec<&str> = Vec::new();
            for (attempted, name) in [
                (do_memory, "memory"),
                (do_wiki, "wiki"),
                (do_tasks, "tasks"),
            ] {
                if !attempted {
                    continue;
                }
                if errors.iter().any(|e| e.starts_with(&format!("{name}:"))) {
                    not_moved.push(name);
                } else {
                    moved.push(name);
                }
            }
            result["manual_intervention_required"] = json!(true);
            result["moved"] = json!(moved);
            result["not_moved"] = json!(not_moved);
            result["warning"] = json!(format!(
                "PARTIAL handoff: '{from_agent}' was NOT archived and is still live \
                 while some data already moved to '{to_agent}'. Manual reconciliation \
                 required (moved: {moved:?}; not moved: {not_moved:?}). This platform \
                 cannot atomically move memory + wiki + tasks together."
            ));
        }

        crate::security_autopilot::audit_and_emit(
            &self.home_dir,
            &duduclaw_security::audit::AuditEvent::new(
                "agent_handoff",
                &from_agent,
                if errors.is_empty() {
                    duduclaw_security::audit::Severity::Warning
                } else {
                    duduclaw_security::audit::Severity::Critical
                },
                json!({
                    "to_agent": to_agent,
                    "memory": do_memory, "wiki": do_wiki, "tasks": do_tasks,
                    "auto_archive": auto_archive,
                    "status": result["status"],
                    "errors": errors,
                }),
            ),
        );

        info!(
            from_agent = from_agent.as_str(),
            to_agent = to_agent.as_str(),
            status = %result["status"],
            "agents.handoff completed"
        );
        WsFrame::ok_response("", result)
    }

    /// Move `from_agent`'s cognitive memory to `to_agent`. Routes to the in-place
    /// re-key when both agents share one DB file, else does a cross-DB move.
    pub(crate) async fn handoff_memory(
        &self,
        from_agent: &str,
        to_agent: &str,
    ) -> Result<duduclaw_memory::ReassignSummary, String> {
        let from_db = self.agent_memory_db_path(from_agent);
        if !from_db.exists() {
            return Ok(duduclaw_memory::ReassignSummary::default());
        }
        let to_db = self.agent_memory_db_path(to_agent);

        let same_file = match (from_db.canonicalize(), to_db.canonicalize()) {
            (Ok(a), Ok(b)) => a == b,
            _ => from_db == to_db,
        };

        if same_file {
            let engine =
                SqliteMemoryEngine::new(&from_db).map_err(|e| format!("open memory db: {e}"))?;
            duduclaw_memory::reassign_agent(&engine, from_agent, to_agent)
                .await
                .map_err(|e| e.to_string())
        } else {
            drop(
                SqliteMemoryEngine::new(&to_db)
                    .map_err(|e| format!("init destination memory db: {e}"))?,
            );
            let from_engine = SqliteMemoryEngine::new(&from_db)
                .map_err(|e| format!("open source memory db: {e}"))?;
            duduclaw_memory::reassign_agent_cross_db(&from_engine, &to_db, from_agent, to_agent)
                .await
                .map_err(|e| e.to_string())
        }
    }

    /// Merge `agents/<from>/wiki` into `agents/<to>/wiki`. Filename collisions
    /// get a numeric suffix (never overwrite). Returns the number of files moved.
    pub(crate) async fn handoff_wiki(&self, from_agent: &str, to_agent: &str) -> Result<u64, String> {
        let agents_dir = self.home_dir.join("agents");
        let from_wiki = agents_dir.join(from_agent).join("wiki");
        if !from_wiki.exists() {
            return Ok(0);
        }
        let to_wiki = agents_dir.join(to_agent).join("wiki");
        tokio::fs::create_dir_all(&to_wiki)
            .await
            .map_err(|e| format!("create destination wiki: {e}"))?;

        let mut moved: u64 = 0;
        let mut stack = vec![from_wiki.clone()];
        while let Some(dir) = stack.pop() {
            let mut rd = match tokio::fs::read_dir(&dir).await {
                Ok(rd) => rd,
                Err(e) => return Err(format!("read wiki dir: {e}")),
            };
            while let Ok(Some(entry)) = rd.next_entry().await {
                let path = entry.path();
                let ft = match entry.file_type().await {
                    Ok(ft) => ft,
                    Err(_) => continue,
                };
                if ft.is_dir() {
                    stack.push(path);
                    continue;
                }
                let rel = match path.strip_prefix(&from_wiki) {
                    Ok(r) => r.to_path_buf(),
                    Err(_) => continue,
                };
                let mut dest = to_wiki.join(&rel);
                if let Some(parent) = dest.parent() {
                    let _ = tokio::fs::create_dir_all(parent).await;
                }
                if dest.exists() {
                    dest = Self::dedupe_dest_path(&dest, from_agent);
                }
                if tokio::fs::rename(&path, &dest).await.is_err() {
                    tokio::fs::copy(&path, &dest)
                        .await
                        .map_err(|e| format!("copy wiki file: {e}"))?;
                    let _ = tokio::fs::remove_file(&path).await;
                }
                moved += 1;
            }
        }
        Ok(moved)
    }

    /// Produce a non-colliding destination path by inserting `-<tag>-<n>` before
    /// the file extension until a free name is found.
    pub(crate) fn dedupe_dest_path(dest: &Path, tag: &str) -> PathBuf {
        let parent = dest.parent().map(|p| p.to_path_buf()).unwrap_or_default();
        let stem = dest
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let ext = dest.extension().map(|s| s.to_string_lossy().into_owned());
        for n in 1..10_000 {
            let name = match &ext {
                Some(e) => format!("{stem}-{tag}-{n}.{e}"),
                None => format!("{stem}-{tag}-{n}"),
            };
            let candidate = parent.join(name);
            if !candidate.exists() {
                return candidate;
            }
        }
        let ts = chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0);
        parent.join(match &ext {
            Some(e) => format!("{stem}-{tag}-{ts}.{e}"),
            None => format!("{stem}-{tag}-{ts}"),
        })
    }
}
