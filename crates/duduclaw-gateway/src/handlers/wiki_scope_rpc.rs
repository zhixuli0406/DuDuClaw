//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    // ── SCP: wiki namespace scope (.scope.toml) ───────────────────────────────
    //
    // Path: `<home>/shared/wiki/.scope.toml` (mirrors duduclaw-cli::wiki_scope).

    /// Read `.scope.toml` through the shared bounded no-follow policy loader:
    /// absent file → empty table (unchanged default), malformed existing file
    /// → `Err` that the caller MUST surface instead of writing anything.
    pub(crate) async fn read_scp_table(&self, path: &Path) -> Result<toml::Table, String> {
        super::wiki_scope_util::load_scp_table(path)
    }

    /// `wiki_scope.get` — read the shared wiki `.scope.toml`. Response:
    /// `{ namespaces: [{ namespace, mode, synced_from, agents }] }`. Absent file → `[]`.
    /// A malformed file is an error (not a silent `[]`) — the admin needs to
    /// know the on-disk policy couldn't be read, not see an empty list that
    /// looks like "nothing configured".
    pub(crate) async fn handle_wiki_scope_get(&self) -> WsFrame {
        let path = self
            .home_dir
            .join("shared")
            .join("wiki")
            .join(".scope.toml");
        match self.read_scp_table(&path).await {
            Ok(table) => WsFrame::ok_response("", scp_table_to_response(&table)),
            Err(e) => WsFrame::error_response("", &e),
        }
    }

    /// `wiki_scope.update` — set (or clear) a single namespace's policy. Params:
    /// `{ namespace, mode: agent_writable|read_only|operator_only|agent_allowlist, synced_from?, agents?,
    /// remove? }`. `remove=true` deletes the entry (reverts to agent_writable
    /// default). Atomic write. Response: `{ success, change }`.
    ///
    /// Fail-closed on a malformed existing file: read happens through
    /// [`Self::read_scp_table`], which returns `Err` instead of silently
    /// treating unparseable content as empty — so this handler bails out
    /// BEFORE calling `scp_apply_namespace`/`atomic_write_toml`, and the
    /// broken file on disk is never touched (previously it would have been
    /// clobbered with a table containing only the one namespace just set).
    pub(crate) async fn handle_wiki_scope_update(&self, params: Value) -> WsFrame {
        let namespace = match params
            .get("namespace")
            .and_then(|v| v.as_str())
            .map(str::trim)
        {
            Some(n) if !n.is_empty() => n.to_string(),
            _ => return WsFrame::error_response("", "Missing 'namespace' parameter"),
        };
        let mode = params
            .get("mode")
            .and_then(|v| v.as_str())
            .unwrap_or("agent_writable");
        let synced_from = params.get("synced_from").and_then(|v| v.as_str());
        let remove = params
            .get("remove")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let path = self
            .home_dir
            .join("shared")
            .join("wiki")
            .join(".scope.toml");
        let mut table = match self.read_scp_table(&path).await {
            Ok(t) => t,
            Err(e) => return WsFrame::error_response("", &e),
        };
        let change = match scp_apply_namespace(&mut table, &namespace, mode, synced_from, remove) {
            Ok(c) => c,
            Err(e) => return WsFrame::error_response("", &e),
        };
        if !remove && mode == "agent_allowlist" {
            let agents = match params.get("agents").and_then(|v| v.as_array()) {
                Some(values) if values.iter().all(|v| v.is_string()) => values.iter()
                    .map(|v| toml::Value::String(v.as_str().unwrap().to_string())).collect(),
                None if params.get("agents").is_none() => Vec::new(),
                _ => return WsFrame::error_response("", "agents must be an array of strings"),
            };
            if let Some(entry) = table.get_mut("namespaces").and_then(|v| v.as_table_mut())
                .and_then(|t| t.get_mut(&namespace)).and_then(|v| v.as_table_mut()) {
                entry.insert("agents".into(), toml::Value::Array(agents));
            }
        }
        let content = match toml::to_string(&table) {
            Ok(content) => content,
            Err(e) => return WsFrame::error_response("", &e.to_string()),
        };
        if let Err(e) = duduclaw_core::scope_policy::ScopePolicy::parse(&content) {
            return WsFrame::error_response("", &e.to_string());
        }
        if let Some(parent) = path.parent() {
            if let Err(e) = tokio::fs::create_dir_all(parent).await {
                return WsFrame::error_response("", &format!("Failed to create wiki dir: {e}"));
            }
        }
        if let Err(e) = duduclaw_memory::wiki_path::atomic_write_nofollow_checked(&path, &content, || {
            duduclaw_core::scope_policy::load(&path).map(|_| ()).map_err(std::io::Error::other)
        }) {
            return WsFrame::error_response("", &e.to_string());
        }
        info!(
            namespace = namespace.as_str(),
            mode, "wiki_scope.update completed"
        );
        WsFrame::ok_response("", json!({ "success": true, "change": change }))
    }
}
