//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    /// Resolve an agent's on-disk directory from the registry.
    pub(crate) async fn resolve_agent_dir(&self, agent_id: &str) -> Result<PathBuf, String> {
        if !is_valid_agent_id(agent_id) {
            return Err(format!("Invalid agent_id: {agent_id}"));
        }
        let reg = self.registry.read().await;
        reg.get(agent_id)
            .map(|a| a.dir.clone())
            .ok_or_else(|| format!("Agent not found: {agent_id}"))
    }

    /// Atomic write of a TOML table to `path` (temp + rename).
    pub(crate) async fn atomic_write_toml(&self, path: &Path, table: &toml::Table) -> Result<(), String> {
        let tmp = path.with_extension("toml.tmp");
        if let Err(e) = self.write_config_table(&tmp, table).await {
            return Err(format!("Failed to write {}: {e}", path.display()));
        }
        if let Err(e) = tokio::fs::rename(&tmp, path).await {
            let _ = tokio::fs::remove_file(&tmp).await;
            return Err(format!("Failed to commit {}: {e}", path.display()));
        }
        Ok(())
    }

    // ── CON: CONTRACT.toml (per-agent) ────────────────────────────────────────

    /// `contract.get` — read `agents/<id>/CONTRACT.toml`.
    /// Params: `{ agent_id }`. Response:
    /// `{ agent_id, must_not[], must_always[], max_tool_calls_per_turn }`.
    pub(crate) async fn handle_contract_get(&self, params: Value) -> WsFrame {
        let agent_id = match params.get("agent_id").and_then(|v| v.as_str()) {
            Some(id) if !id.is_empty() => id.to_string(),
            _ => return WsFrame::error_response("", "Missing 'agent_id' parameter"),
        };
        let dir = match self.resolve_agent_dir(&agent_id).await {
            Ok(d) => d,
            Err(e) => return WsFrame::error_response("", &e),
        };
        let path = dir.join("CONTRACT.toml");
        let table = self.read_config_table(&path).await;
        let mut resp = contract_table_to_response(&table);
        if let Some(obj) = resp.as_object_mut() {
            obj.insert("agent_id".into(), json!(agent_id));
        }
        WsFrame::ok_response("", resp)
    }

    /// `contract.update` — atomic write of `agents/<id>/CONTRACT.toml`.
    /// Params: `{ agent_id, must_not[], must_always[], max_tool_calls_per_turn }`.
    /// Response: `{ success, agent_id, must_not[], must_always[], max_tool_calls_per_turn }`.
    pub(crate) async fn handle_contract_update(&self, params: Value) -> WsFrame {
        let agent_id = match params.get("agent_id").and_then(|v| v.as_str()) {
            Some(id) if !id.is_empty() => id.to_string(),
            _ => return WsFrame::error_response("", "Missing 'agent_id' parameter"),
        };
        let dir = match self.resolve_agent_dir(&agent_id).await {
            Ok(d) => d,
            Err(e) => return WsFrame::error_response("", &e),
        };
        let table = match build_contract_table(&params) {
            Ok(t) => t,
            Err(e) => return WsFrame::error_response("", &e),
        };
        let path = dir.join("CONTRACT.toml");
        if let Err(e) = self.atomic_write_toml(&path, &table).await {
            return WsFrame::error_response("", &e);
        }
        info!(agent_id = agent_id.as_str(), "contract.update completed");
        // The channel-side FYI — the boundary changed but only applies
        // starting the agent's next turn (see the `message` field below), so
        // mark it pending; the channel reply pipeline appends a one-line
        // notice once, then clears it (`pending_agent_notice`).
        crate::pending_agent_notice::mark_contract_changed(&self.home_dir, &agent_id);
        let mut resp = contract_table_to_response(&table);
        if let Some(obj) = resp.as_object_mut() {
            obj.insert("success".into(), json!(true));
            obj.insert("agent_id".into(), json!(agent_id));
            // The contract is loaded per-invocation from disk by the agent
            // runner, so the next turn picks up the new boundaries automatically.
            obj.insert(
                "message".into(),
                json!("Contract updated — applies on the agent's next turn"),
            );
        }
        WsFrame::ok_response("", resp)
    }
}
