//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    // ── SCP: wiki namespace scope (.scope.toml) ───────────────────────────────
    //
    // Path: `<home>/shared/wiki/.scope.toml` (mirrors duduclaw-cli::wiki_scope).

    /// Read `.scope.toml` for the SCP handlers via [`parse_scp_table_strict`]:
    /// absent file → empty table (unchanged default), malformed existing file
    /// → `Err` that the caller MUST surface instead of writing anything.
    pub(crate) async fn read_scp_table(&self, path: &Path) -> Result<toml::Table, String> {
        match tokio::fs::read_to_string(path).await {
            Ok(content) => parse_scp_table_strict(&content),
            Err(_) => Ok(toml::Table::new()),
        }
    }

    /// `wiki_scope.get` — read the shared wiki `.scope.toml`. Response:
    /// `{ namespaces: [{ namespace, mode, synced_from }] }`. Absent file → `[]`.
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
    /// `{ namespace, mode: agent_writable|read_only|operator_only, synced_from?,
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
        if let Some(parent) = path.parent() {
            if let Err(e) = tokio::fs::create_dir_all(parent).await {
                return WsFrame::error_response("", &format!("Failed to create wiki dir: {e}"));
            }
        }
        if let Err(e) = self.atomic_write_toml(&path, &table).await {
            return WsFrame::error_response("", &e);
        }
        info!(
            namespace = namespace.as_str(),
            mode, "wiki_scope.update completed"
        );
        WsFrame::ok_response("", json!({ "success": true, "change": change }))
    }
}
