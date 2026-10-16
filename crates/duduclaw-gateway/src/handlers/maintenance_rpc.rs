//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    // ── Maintenance Mode — Entry A ────────────────────────────
    // `commercial/docs/specs/DESIGN-maintenance-mode-2026-08.md` §2. Dispatch gates
    // (Admin-only + appliance-only) live in `dispatch()`'s method match, same
    // as every other `device.*`/`network.*` handler; these methods are the
    // orchestration + re-auth/confirm checks specific to this RPC family.

    /// The maintenance lifecycle environment: the real sysd and notices to
    /// every Admin of the user database.
    async fn maintenance_env(&self) -> crate::maintenance::MaintEnv {
        let db = self.user_db.read().await.clone();
        crate::maintenance::MaintEnv::system(self.home_dir(), db)
    }

    /// `maintenance.enable` — §2.1 two layers of confirmation ON TOP OF the
    /// dispatch-level Admin-only gate: an exact type-to-confirm string
    /// (mirrors `device.factory_reset`'s `"RESET"` convention) AND a
    /// step-up re-auth (the caller's OWN current password). Since 2026-10
    /// (`DESIGN-maintenance-ssh-account-2026-10.md` §4.1, §6.1) the `ssh`
    /// sub-capability needs `ssh_public_key` in the same request, and five
    /// wrong passwords in a row lock opening for fifteen minutes (checked
    /// before the password is verified). Refusals carry a stable `code`
    /// (`maintenance::error_code`) next to a zh-TW `message`.
    pub(crate) async fn handle_maintenance_enable(&self, params: Value, ctx: &UserContext) -> WsFrame {
        use crate::maintenance::{EnableRequest, MaintError, error_code};

        if let Err(e) = require_maintenance_writer(self.home_dir()) {
            return maintenance_error(&e);
        }
        let confirm_text = params
            .get("confirm_text")
            .and_then(Value::as_str)
            .unwrap_or("");
        if confirm_text != crate::maintenance::CONFIRM_TEXT {
            return maintenance_error(&MaintError::new(
                error_code::CONFIRM_TEXT_MISMATCH,
                format!("請輸入 {} 確認。", crate::maintenance::CONFIRM_TEXT),
            ));
        }
        let db = match self.user_db.read().await.as_ref() {
            Some(db) => db.clone(),
            None => {
                return maintenance_error(&MaintError::new(
                    error_code::USER_SYSTEM_UNAVAILABLE,
                    "帳號系統尚未啟動，無法驗證密碼。",
                ));
            }
        };
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
        let ssh_public_key = params.get("ssh_public_key").and_then(Value::as_str).map(str::to_string);
        let reauth_password = params.get("reauth_password").and_then(Value::as_str).unwrap_or("").to_string();
        let env = std::sync::Arc::new(self.maintenance_env().await);
        // Run to completion even if this RPC future is dropped (client gone):
        // an `opening` row and a sysd open must not be abandoned halfway.
        let (task_env, task_db) = (env.clone(), db.clone());
        let (user_id, email) = (ctx.user_id.clone(), ctx.email.clone());
        let outcome = tokio::spawn(async move {
            let request = EnableRequest {
                user_id: &user_id,
                email: &email,
                ttl_hours,
                sub_capabilities: &sub_capabilities,
                ssh_public_key: ssh_public_key.as_deref(),
                reauth_password: &reauth_password,
            };
            crate::maintenance::enable(&task_env, &request, task_db.as_ref()).await
        })
        .await
        .unwrap_or_else(|e| Err(MaintError::new(error_code::INTERNAL, format!("維修模式暫時無法操作，請稍後再試。（{e}）"))));
        match outcome {
            Ok(window) => {
                let status = crate::maintenance::status_json(&env, None)
                    .await
                    .unwrap_or_else(|_| json!({"active": true}));
                self.broadcast_event("maintenance.status_changed", status)
                    .await;
                WsFrame::ok_response("", json!({ "window": window }))
            }
            Err(e) => {
                if e.code == error_code::REAUTH_FAILED || e.code == error_code::REAUTH_LOCKED {
                    let _ = db.log_action(
                        Some(&ctx.user_id),
                        "maintenance.enable_reauth_failed",
                        Some(&ctx.user_id),
                        Some(e.code),
                        None,
                    );
                }
                maintenance_error(&e)
            }
        }
    }

    /// `maintenance.disable` — no confirm/re-auth: turning OFF a sensitive
    /// mode is the safe direction, matching every other danger-zone RPC in
    /// this codebase (only the destructive/opening action requires
    /// confirmation). The returned window carries `close_error` when the SSH
    /// close could not be confirmed.
    pub(crate) async fn handle_maintenance_disable(&self, params: Value, ctx: &UserContext) -> WsFrame {
        if let Err(e) = require_maintenance_writer(self.home_dir()) {
            return maintenance_error(&e);
        }
        let reason = params.get("reason").and_then(Value::as_str);
        let env = self.maintenance_env().await;
        match crate::maintenance::disable(&env, &ctx.user_id, reason).await {
            Ok(window) => {
                let status = crate::maintenance::status_json(&env, None)
                    .await
                    .unwrap_or_else(|_| json!({"active": false}));
                self.broadcast_event("maintenance.status_changed", status)
                    .await;
                WsFrame::ok_response("", json!({ "window": window }))
            }
            Err(e) => WsFrame::error_response("", &e),
        }
    }

    /// `maintenance.status` — current state, what sysd reports about the SSH
    /// window (always asked, design M2), and the caller's re-auth lock.
    pub(crate) async fn handle_maintenance_status(&self, ctx: &UserContext) -> WsFrame {
        let env = self.maintenance_env().await;
        match crate::maintenance::status_json(&env, Some(&ctx.user_id)).await {
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

/// Only the gateway running this home's supervisor can mutate its windows.
/// A second gateway may still serve status, but its process-local window
/// mutex cannot serialise writes with the owner process.
fn require_maintenance_writer(home: &std::path::Path) -> Result<(), crate::maintenance::MaintError> {
    if duduclaw_core::gateway_instance::held(home) {
        return Ok(());
    }
    Err(crate::maintenance::MaintError::new(
        crate::maintenance::error_code::GATEWAY_NOT_OWNER,
        "這個閘道目前無法變更維修模式，請使用正在運作的閘道。",
    ))
}

#[cfg(test)]
mod writer_tests {
    use super::*;
    use fs2::FileExt;

    #[test]
    fn maintenance_writer_requires_the_real_lock_for_its_own_home() {
        let owner_home = tempfile::tempdir().unwrap();
        let other_home = tempfile::tempdir().unwrap();
        assert!(require_maintenance_writer(owner_home.path()).is_err());
        duduclaw_core::gateway_instance::acquire(owner_home.path()).unwrap();
        assert!(require_maintenance_writer(owner_home.path()).is_ok());
        assert!(require_maintenance_writer(other_home.path()).is_err());

        // An independently held OS lock stands in for the other gateway.
        std::fs::create_dir_all(other_home.path().join("locks")).unwrap();
        let other = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(duduclaw_core::gateway_instance::lock_path(other_home.path()))
            .unwrap();
        other.try_lock_exclusive().unwrap();
        assert!(duduclaw_core::gateway_instance::acquire(other_home.path()).is_err());
        assert_eq!(
            require_maintenance_writer(other_home.path()).unwrap_err().code,
            crate::maintenance::error_code::GATEWAY_NOT_OWNER
        );
        assert!(require_maintenance_writer(owner_home.path()).is_ok());
    }
}

/// An error frame whose `error` is `{"code", "message", ...}` (the dashboard
/// reads `message` for display and branches on `code`).
fn maintenance_error(e: &crate::maintenance::MaintError) -> WsFrame {
    WsFrame::Response {
        id: String::new(),
        ok: false,
        payload: None,
        error: Some(e.to_json()),
    }
}
