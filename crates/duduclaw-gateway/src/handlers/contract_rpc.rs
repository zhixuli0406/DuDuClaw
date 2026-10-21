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
        if path.file_name().and_then(|n| n.to_str()) == Some("config.toml") {
            let current = super::config_commit::read_text_or_empty(path)?;
            let current_table = if current.trim().is_empty() {
                toml::Table::new()
            } else {
                current.parse::<toml::Table>()
                    .map_err(|e| format!("Invalid config.toml; refusing edit: {e}"))?
            };
            if current_table.get("accounts") != table.get("accounts") {
                return Err("config.toml accounts changed while this edit was open; reload and retry".into());
            }
            return super::config_commit::commit_table_locked(
                path,
                super::config_commit::content_hash(&current),
                table,
            ).await;
        }
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
            // Send back as `base_hash` on `contract.update`.
            let text = crate::agent_files::read_or_empty(&path).unwrap_or_default();
            obj.insert("hash".into(), json!(crate::agent_files::hash_of(&text)));
        }
        WsFrame::ok_response("", resp)
    }
    // `contract.update` lives in `agent_files_rpc.rs` (the profile file writer
    // and the strict check).
}
