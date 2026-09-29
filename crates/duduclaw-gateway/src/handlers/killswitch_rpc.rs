//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    // ── KS: global ~/.duduclaw/KILLSWITCH.toml ────────────────────────────────

    /// `killswitch.get` — read the global `~/.duduclaw/KILLSWITCH.toml`, filling
    /// any missing field with the documented default so the form is complete.
    pub(crate) async fn handle_killswitch_get(&self) -> WsFrame {
        let path = self.home_dir.join("KILLSWITCH.toml");
        let table = self.read_config_table(&path).await;
        WsFrame::ok_response("", killswitch_table_to_response(&table))
    }

    /// `killswitch.update` — atomic write of `~/.duduclaw/KILLSWITCH.toml`.
    /// Params (all sub-sections optional, partial update):
    /// `{ triggers{}, circuit_breaker{}, failsafe{}, safety_words{},
    /// defensive_prompt{}, audit{} }`. Response: `{ success, changes[] }`.
    pub(crate) async fn handle_killswitch_update(&self, params: Value) -> WsFrame {
        let path = self.home_dir.join("KILLSWITCH.toml");
        let mut table = self.read_config_table(&path).await;
        let changes = match apply_killswitch_to_table(&mut table, &params) {
            Ok(c) => c,
            Err(e) => return WsFrame::error_response("", &e),
        };
        if changes.is_empty() {
            return WsFrame::error_response("", "No valid killswitch fields to update");
        }
        if let Err(e) = self.atomic_write_toml(&path, &table).await {
            return WsFrame::error_response("", &e);
        }
        info!(?changes, "killswitch.update completed");
        WsFrame::ok_response(
            "",
            json!({
                "success": true,
                "changes": changes,
                "message": "Kill switch updated — most thresholds apply on gateway restart",
            }),
        )
    }
}
