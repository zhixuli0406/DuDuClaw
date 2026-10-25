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
            return Err("config.toml requires the original read snapshot".into());
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

    pub(crate) async fn read_toml_write_snapshot(
        &self,
        path: &Path,
    ) -> Result<(toml::Table, super::config_commit::HomeTableSnapshot), String> {
        if path != self.home_dir.join("config.toml").as_path() {
            return Err("Home snapshot target must be this handler's config.toml".into());
        }
        super::config_commit::read_home_table_snapshot(path)
    }

    pub(crate) async fn atomic_write_toml_snapshot(
        &self,
        path: &Path,
        table: &toml::Table,
        snapshot: super::config_commit::HomeTableSnapshot,
    ) -> Result<(), String> {
        if path != self.home_dir.join("config.toml").as_path() {
            return Err("Home snapshot target must be this handler's config.toml".into());
        }
        let path = path.to_path_buf();
        let table = table.clone();
        tokio::task::spawn_blocking(move || {
            super::config_commit::commit_home_table_snapshot(&path, snapshot, &table)
        })
        .await
        .map_err(|e| format!("config write task failed: {e}"))?
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
