//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    // ── Maintenance Mode — Entry A ────────────────────────────
    // `commercial/docs/DESIGN-maintenance-mode-2026-08.md` §2. Dispatch gates
    // (Admin-only + appliance-only) live in `dispatch()`'s method match, same
    // as every other `device.*`/`network.*` handler; these methods are the
    // orchestration + re-auth/confirm checks specific to this RPC family.

    /// `maintenance.enable` — §2.1 two layers of confirmation ON TOP OF the
    /// dispatch-level Admin-only gate: an exact type-to-confirm string
    /// (mirrors `device.factory_reset`'s `"RESET"` convention) AND a
    /// step-up re-auth (the caller's OWN current password, verified via the
    /// same `UserDb::verify_password` `users.change_password` already uses —
    /// §8 Q1 resolved in favor of requiring it, since the mechanism turned
    /// out to already exist rather than needing new session-model surgery).
    pub(crate) async fn handle_maintenance_enable(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let confirm_text = params
            .get("confirm_text")
            .and_then(Value::as_str)
            .unwrap_or("");
        if confirm_text != crate::maintenance::CONFIRM_TEXT {
            return WsFrame::error_response(
                "",
                &format!(
                    "confirm_text must be exactly \"{}\"",
                    crate::maintenance::CONFIRM_TEXT
                ),
            );
        }

        let reauth_password = params
            .get("reauth_password")
            .and_then(Value::as_str)
            .unwrap_or("");
        let db = match self.user_db.read().await.as_ref() {
            Some(db) => db.clone(),
            None => return WsFrame::error_response("", "user system not initialized"),
        };
        if db.verify_password(&ctx.email, reauth_password).is_err() {
            let _ = db.log_action(
                Some(&ctx.user_id),
                "maintenance.enable_reauth_failed",
                Some(&ctx.user_id),
                None,
                None,
            );
            return WsFrame::error_response("", "re-auth password is incorrect");
        }

        let ttl_hours = params
            .get("ttl_hours")
            .and_then(Value::as_i64)
            .unwrap_or(crate::maintenance::DEFAULT_TTL_HOURS);
        let sub_capabilities: Vec<String> = params
            .get("sub_capabilities")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();

        match crate::maintenance::enable(
            self.home_dir(),
            &ctx.user_id,
            &ctx.email,
            ttl_hours,
            &sub_capabilities,
        )
        .await
        {
            Ok(window) => {
                let status = crate::maintenance::status_json(self.home_dir())
                    .await
                    .unwrap_or_else(|_| json!({"active": true}));
                self.broadcast_event("maintenance.status_changed", status)
                    .await;
                WsFrame::ok_response("", json!({ "window": window }))
            }
            Err(e) => WsFrame::error_response("", &e),
        }
    }

    /// `maintenance.disable` — no confirm/re-auth: turning OFF a sensitive
    /// mode is the safe direction, matching every other danger-zone RPC in
    /// this codebase (only the destructive/opening action requires
    /// confirmation).
    pub(crate) async fn handle_maintenance_disable(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let reason = params.get("reason").and_then(Value::as_str);
        match crate::maintenance::disable(self.home_dir(), &ctx.user_id, reason).await {
            Ok(window) => {
                let status = crate::maintenance::status_json(self.home_dir())
                    .await
                    .unwrap_or_else(|_| json!({"active": false}));
                self.broadcast_event("maintenance.status_changed", status)
                    .await;
                WsFrame::ok_response("", json!({ "window": window }))
            }
            Err(e) => WsFrame::error_response("", &e),
        }
    }

    /// `maintenance.status` — current state + the live SSH probe. Read-only,
    /// no confirm needed.
    pub(crate) async fn handle_maintenance_status(&self) -> WsFrame {
        match crate::maintenance::status_json(self.home_dir()).await {
            Ok(status) => WsFrame::ok_response("", status),
            Err(e) => WsFrame::error_response("", &e),
        }
    }

    /// `maintenance.history` — paginated JSONL audit trail (§2.8: `limit`
    /// default 100, capped at 1000, mirroring the `cloud-control-plane`
    /// `audit::list` convention this design cites).
    pub(crate) fn handle_maintenance_history(&self, params: Value) -> WsFrame {
        let limit = params
            .get("limit")
            .and_then(Value::as_u64)
            .map(|v| v.min(1000) as usize)
            .unwrap_or(100);
        let offset = params.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize;
        match crate::maintenance::history_json(self.home_dir(), limit, offset) {
            Ok(history) => WsFrame::ok_response("", history),
            Err(e) => WsFrame::error_response("", &e),
        }
    }

    /// `maintenance.log_access` — §2.3's "每一次「顯示詳情」還原原文...各自
    /// 獨立記一筆" requirement. Called by the dashboard the moment an
    /// operator expands a `device.*` op's full raw output while maintenance
    /// mode's `show_details` sub-capability is unlocked; refuses (via
    /// `maintenance::log_access`'s own fail-closed check) if that is not
    /// actually true, so a stray call cannot manufacture a false audit line.
    pub(crate) async fn handle_maintenance_log_access(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let operation = params
            .get("operation")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let mut fields = serde_json::Map::new();
        fields.insert("operation".to_string(), json!(operation));
        match crate::maintenance::log_access(
            self.home_dir(),
            &ctx.user_id,
            &ctx.email,
            "access_view_detail",
            fields,
        )
        .await
        {
            Ok(()) => WsFrame::ok_response("", json!({"logged": true})),
            Err(e) => WsFrame::error_response("", &e),
        }
    }
}
