//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    /// Upload an agent avatar image (WP4). Accepts a PNG/JPEG/WebP data URI,
    /// validates the declared mime against real magic bytes, enforces a 512 KB
    /// decoded ceiling, and stores the raw bytes at `agents/<id>/avatar.<ext>`
    /// (atomic temp + rename). Any prior avatar of a different extension is
    /// removed so exactly one `avatar.*` exists.
    /// `agents.set_outfit` — save (or clear, with `outfit: null`) the agent's
    /// wardrobe composition. The outfit is a small slot→item-id map rendered
    /// client-side (SVG roster + PixiJS world); the server validates shape and
    /// vocabulary-safe characters, then persists `agents/<id>/outfit.json`
    /// atomically. It never affects agent behaviour — purely cosmetic.
    pub(crate) async fn handle_agents_set_outfit(&self, params: Value) -> WsFrame {
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
        let agent_dir = self.home_dir.join("agents").join(&agent_id);
        let outfit_path = agent_dir.join("outfit.json");

        let Some(outfit) = params.get("outfit") else {
            return WsFrame::error_response("", "Missing 'outfit' parameter (object or null)");
        };
        // `outfit: null` clears the saved look (back to the seeded default).
        if outfit.is_null() {
            if outfit_path.exists() {
                if let Err(e) = tokio::fs::remove_file(&outfit_path).await {
                    return WsFrame::error_response("", &format!("Failed to clear outfit: {e}"));
                }
            }
            return WsFrame::ok_response(
                "",
                json!({ "success": true, "agent_id": agent_id, "outfit": Value::Null }),
            );
        }
        let normalized = match normalize_outfit(outfit) {
            Ok(v) => v,
            Err(e) => return WsFrame::error_response("", &e),
        };

        let body = serde_json::to_string_pretty(&normalized).unwrap_or_default();
        let tmp = outfit_path.with_extension("json.tmp");
        if let Err(e) = tokio::fs::write(&tmp, &body).await {
            return WsFrame::error_response("", &format!("Failed to write outfit: {e}"));
        }
        if let Err(e) = tokio::fs::rename(&tmp, &outfit_path).await {
            let _ = tokio::fs::remove_file(&tmp).await;
            return WsFrame::error_response("", &format!("Failed to commit outfit: {e}"));
        }
        info!(agent = %agent_id, "agent outfit saved");
        WsFrame::ok_response(
            "",
            json!({ "success": true, "agent_id": agent_id, "outfit": normalized }),
        )
    }

    pub(crate) async fn handle_agents_set_avatar(&self, params: Value) -> WsFrame {
        let agent_id = match params.get("agent_id").and_then(|v| v.as_str()) {
            Some(id) if !id.is_empty() => id.to_string(),
            _ => return WsFrame::error_response("", "Missing 'agent_id' parameter"),
        };
        if !is_valid_agent_id(&agent_id) {
            return WsFrame::error_response("", "Invalid agent_id format");
        }
        let data_uri = match params.get("data_uri").and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => s,
            _ => return WsFrame::error_response("", "Missing 'data_uri' parameter"),
        };
        {
            let reg = self.registry.read().await;
            if reg.get(&agent_id).is_none() {
                return WsFrame::error_response("", &format!("Agent not found: {agent_id}"));
            }
        }

        let (bytes, ext) = match decode_avatar_data_uri(data_uri) {
            Ok(v) => v,
            Err(e) => return WsFrame::error_response("", &e),
        };

        let agent_dir = self.home_dir.join("agents").join(&agent_id);
        if !agent_dir.exists() {
            return WsFrame::error_response("", &format!("Agent directory missing: {agent_id}"));
        }
        // F8: write the NEW avatar first (temp → validate → atomic rename), and
        // only remove the old avatars of a *different* extension AFTER the new
        // one is committed. The previous order deleted every avatar.* first, so
        // a failed write left the agent with no image at all.
        let dest = agent_dir.join(format!("avatar.{ext}"));
        let tmp = agent_dir.join(format!("avatar.{ext}.tmp"));
        if let Err(e) = tokio::fs::write(&tmp, &bytes).await {
            return WsFrame::error_response("", &format!("Failed to write avatar: {e}"));
        }
        if let Err(e) = tokio::fs::rename(&tmp, &dest).await {
            let _ = tokio::fs::remove_file(&tmp).await;
            return WsFrame::error_response("", &format!("Failed to commit avatar: {e}"));
        }
        // New avatar is safely in place; drop stale avatars of other extensions
        // so exactly one avatar.* remains.
        for e in AVATAR_EXTS {
            if *e != ext {
                let _ = tokio::fs::remove_file(agent_dir.join(format!("avatar.{e}"))).await;
            }
        }

        info!(
            agent_id = agent_id.as_str(),
            bytes = bytes.len(),
            ext,
            "Agent avatar set"
        );
        WsFrame::ok_response(
            "",
            json!({
                "success": true,
                "agent_id": agent_id,
                "has_avatar": true,
                "bytes": bytes.len(),
            }),
        )
    }

    /// Remove an agent's uploaded avatar (WP4). No-op-safe.
    pub(crate) async fn handle_agents_clear_avatar(&self, params: Value) -> WsFrame {
        let agent_id = match params.get("agent_id").and_then(|v| v.as_str()) {
            Some(id) if !id.is_empty() => id.to_string(),
            _ => return WsFrame::error_response("", "Missing 'agent_id' parameter"),
        };
        if !is_valid_agent_id(&agent_id) {
            return WsFrame::error_response("", "Invalid agent_id format");
        }
        let agent_dir = self.home_dir.join("agents").join(&agent_id);
        let mut removed = false;
        for e in AVATAR_EXTS {
            if tokio::fs::remove_file(agent_dir.join(format!("avatar.{e}")))
                .await
                .is_ok()
            {
                removed = true;
            }
        }
        info!(
            agent_id = agent_id.as_str(),
            removed, "Agent avatar cleared"
        );
        WsFrame::ok_response(
            "",
            json!({
                "success": true,
                "agent_id": agent_id,
                "has_avatar": false,
                "removed": removed,
            }),
        )
    }

    /// Read an agent's stored avatar (if any) back into a data URI. Mirrors the
    /// branding-logo model (inline data URI) so the front-end renders it under
    /// the same `img-src data:` CSP. `None` when no `avatar.*` file exists.
    pub(crate) fn agent_avatar_data_uri(&self, agent_id: &str) -> Option<String> {
        let agent_dir = self.home_dir.join("agents").join(agent_id);
        for (ext, mime) in AVATAR_EXT_MIME {
            let path = agent_dir.join(format!("avatar.{ext}"));
            if let Ok(bytes) = std::fs::read(&path) {
                use base64::{Engine, engine::general_purpose::STANDARD as B64};
                return Some(format!("data:{mime};base64,{}", B64.encode(&bytes)));
            }
        }
        None
    }

    /// Whether an agent has an uploaded avatar file on disk.
    ///
    /// E3: `agents.list` is a roster polling RPC that calls this once per agent,
    /// so it must be cheap. A single `read_dir` replaces the former up-to-3
    /// `Path::exists` stat syscalls — we scan the directory once and match any
    /// `avatar.<ext>` where `<ext>` is a supported image extension.
    pub(crate) fn agent_has_avatar(&self, agent_id: &str) -> bool {
        let agent_dir = self.home_dir.join("agents").join(agent_id);
        let Ok(rd) = std::fs::read_dir(&agent_dir) else {
            return false;
        };
        for entry in rd.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if let Some(ext) = name.strip_prefix("avatar.") {
                if AVATAR_EXTS.contains(&ext) {
                    return true;
                }
            }
        }
        false
    }

    /// E1: lightweight avatar fetch. The roster/sidebar/chat all render
    /// `CharacterAvatar` for every AI staff member, and the avatar store resolves
    /// uploaded bytes one agent at a time. Routing that through `agents.inspect`
    /// paid for a telemetry month-to-date aggregate + full SOUL/identity/skills/
    /// model-config serialization on every first-paint avatar — N heavy RPCs just
    /// to read one image. This handler reads only `agents/<id>/avatar.<ext>` and
    /// returns it as an inline data URI (or null). Agent visibility is enforced at
    /// dispatch by `check_agent!(Viewer)` (fail-closed), same gate as `inspect`.
    pub(crate) async fn handle_agents_avatar(&self, params: Value) -> WsFrame {
        let agent_id = params
            .get("agent_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if agent_id.is_empty() {
            return WsFrame::error_response("", "Missing 'agent_id' parameter");
        }
        let avatar = self.agent_avatar_data_uri(agent_id);
        WsFrame::ok_response(
            "",
            json!({
                "agent_id": agent_id,
                "has_avatar": avatar.is_some(),
                "avatar": avatar,
            }),
        )
    }
}
