//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    // ── WP-D: "訂閱帳號" setup-token wizard ────────────────────
    //
    // A guided, single-flight specialization of "Dashboard 一鍵登入" above,
    // scoped to Claude subscription accounts on headless boxes (`claude
    // setup-token`'s paste-back flow). See `setup_token_wizard.rs` for the
    // orchestration + the real-CLI-output findings behind its design.

    pub(crate) async fn handle_accounts_setup_token_start(&self) -> WsFrame {
        let Some(claude_bin) =
            crate::cli_auth::resolve_program(duduclaw_core::types::RuntimeType::Claude)
        else {
            return setup_token_error_frame(
                crate::setup_token_wizard::SetupTokenErrorCode::NotInstalled,
                "找不到 claude CLI，請先安裝 Claude Code。",
            );
        };

        // Single concurrency: a second `start` cancels whatever is currently
        // in progress (WP-D spec) rather than erroring — the previous flow's
        // browser tab, if any, was abandoned anyway.
        {
            let mut guard = self.setup_token_session.write().await;
            if let Some(old) = guard.take() {
                info!(
                    target: "setup_token_wizard",
                    old_session = %old.session_id,
                    "start: cancelling previous setup-token session"
                );
                old.session.kill();
            }
        }

        let session_id = uuid::Uuid::new_v4().simple().to_string();
        let slot = match crate::setup_token_wizard::spawn_session(session_id.clone()) {
            Ok(s) => s,
            Err(crate::cli_auth::AuthError::NotInstalled) => {
                return setup_token_error_frame(
                    crate::setup_token_wizard::SetupTokenErrorCode::NotInstalled,
                    "找不到 claude CLI，請先安裝 Claude Code。",
                );
            }
            Err(e) => {
                warn!(target: "setup_token_wizard", error = %e, "failed to spawn setup-token session");
                return setup_token_error_frame(
                    crate::setup_token_wizard::SetupTokenErrorCode::Io,
                    "無法啟動連接流程，請稍後再試。",
                );
            }
        };

        // Bounded wait so the RPC returns the URL synchronously in the
        // common case; the dashboard falls back to polling `status` if the
        // CLI hasn't printed it yet.
        let auth_url = crate::setup_token_wizard::wait_for_auth_url(&slot.session).await;

        *self.setup_token_session.write().await = Some(slot.clone());

        WsFrame::ok_response(
            "",
            json!({
                "session_id": slot.session_id,
                "auth_url": auth_url,
                "expires_in_seconds": slot.expires_in_seconds(),
                "program": claude_bin,
            }),
        )
    }

    pub(crate) async fn handle_accounts_setup_token_status(&self, params: Value) -> WsFrame {
        let session_id = params
            .get("session_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let guard = self.setup_token_session.read().await;
        let Some(slot) = guard.as_ref().filter(|s| s.session_id == session_id) else {
            return setup_token_error_frame(
                crate::setup_token_wizard::SetupTokenErrorCode::NoActiveSession,
                "找不到這個連接流程，請重新開始。",
            );
        };
        if slot.is_expired() {
            let session_id = slot.session_id.clone();
            let session = slot.session.clone();
            drop(guard);
            self.clear_setup_token_slot(&session_id).await;
            session.kill();
            return setup_token_error_frame(
                crate::setup_token_wizard::SetupTokenErrorCode::Expired,
                "連接流程已逾時（5 分鐘），請重新開始。",
            );
        }
        WsFrame::ok_response(
            "",
            json!({
                "session_id": slot.session_id,
                "status": slot.session.status().as_str(),
                "auth_url": slot.session.captured_auth_url(),
                "expires_in_seconds": slot.expires_in_seconds(),
            }),
        )
    }

    pub(crate) async fn handle_accounts_setup_token_cancel(&self, params: Value) -> WsFrame {
        let session_id = params
            .get("session_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if let Some(slot) = self.clear_setup_token_slot(session_id).await {
            slot.session.kill();
        }
        WsFrame::ok_response("", json!({"success": true}))
    }

    /// Remove the setup-token slot if `session_id` matches the current one —
    /// never clobbers a NEWER session that replaced it. Returns the removed
    /// slot (so the caller can `kill()` it after dropping the lock).
    pub(crate) async fn clear_setup_token_slot(
        &self,
        session_id: &str,
    ) -> Option<crate::setup_token_wizard::SetupTokenSlot> {
        let mut guard = self.setup_token_session.write().await;
        if guard.as_ref().is_some_and(|s| s.session_id == session_id) {
            guard.take()
        } else {
            None
        }
    }

    /// Paste the code back, wait synchronously for the CLI to finish, and —
    /// on success — validate the captured token with a real API round-trip
    /// BEFORE persisting it via the existing `accounts.add` path. The token
    /// itself never appears in the response or in any log line here (see
    /// `setup_token_wizard.rs` module doc).
    pub(crate) async fn handle_accounts_setup_token_submit(&self, params: Value) -> WsFrame {
        let session_id = params
            .get("session_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let code = match params.get("code").and_then(|v| v.as_str()) {
            Some(c) if !c.trim().is_empty() => c,
            _ => {
                return setup_token_error_frame(
                    crate::setup_token_wizard::SetupTokenErrorCode::InvalidCode,
                    "請輸入授權碼。",
                );
            }
        };

        let slot = {
            let guard = self.setup_token_session.read().await;
            match guard.as_ref().filter(|s| s.session_id == session_id) {
                Some(s) => s.clone(),
                None => {
                    return setup_token_error_frame(
                        crate::setup_token_wizard::SetupTokenErrorCode::NoActiveSession,
                        "找不到這個連接流程，請重新開始。",
                    );
                }
            }
        };
        if slot.is_expired() {
            self.clear_setup_token_slot(&slot.session_id).await;
            slot.session.kill();
            return setup_token_error_frame(
                crate::setup_token_wizard::SetupTokenErrorCode::Expired,
                "連接流程已逾時（5 分鐘），請重新開始。",
            );
        }
        if !slot.try_begin_submit() {
            return setup_token_error_frame(
                crate::setup_token_wizard::SetupTokenErrorCode::AlreadySubmitting,
                "已經在處理上一次送出的授權碼，請稍候。",
            );
        }

        let claude_bin =
            match crate::cli_auth::resolve_program(duduclaw_core::types::RuntimeType::Claude) {
                Some(b) => b,
                None => {
                    self.clear_setup_token_slot(&slot.session_id).await;
                    return setup_token_error_frame(
                        crate::setup_token_wizard::SetupTokenErrorCode::NotInstalled,
                        "找不到 claude CLI，請先安裝 Claude Code。",
                    );
                }
            };

        let outcome =
            crate::setup_token_wizard::submit_code(&claude_bin, &slot.session, code).await;

        // Every outcome (success or failure) retires this slot — a fresh
        // `start` is required to try again, matching the CLI's own terminal
        // status semantics (see `SetupTokenSlot::try_begin_submit` doc).
        self.clear_setup_token_slot(&slot.session_id).await;

        match outcome {
            crate::setup_token_wizard::SubmitOutcome::Validated(token) => {
                let secs = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                let id = format!("claude-subscription-{secs}");
                let add = json!({
                    "id": id,
                    "type": "oauth",
                    "key": token,
                    "priority": 1,
                    "monthly_budget_cents": 0,
                });
                let res = self.handle_accounts_add(add).await;
                crate::claude_runner::invalidate_rotator_cache().await;
                match res {
                    WsFrame::Response { ok: true, .. } => {
                        info!(target: "setup_token_wizard", account = %id, "subscription account registered");
                        WsFrame::ok_response("", json!({"success": true, "account_id": id}))
                    }
                    other => other, // propagate the accounts.add error verbatim
                }
            }
            crate::setup_token_wizard::SubmitOutcome::NoToken => setup_token_error_frame(
                crate::setup_token_wizard::SetupTokenErrorCode::ValidationFailed,
                "登入流程完成了，但沒有取得授權憑證，請重新開始。",
            ),
            crate::setup_token_wizard::SubmitOutcome::ValidationFailed(reason) => {
                warn!(target: "setup_token_wizard", reason = %reason, "captured token failed live validation");
                setup_token_error_frame(
                    crate::setup_token_wizard::SetupTokenErrorCode::ValidationFailed,
                    &reason,
                )
            }
            crate::setup_token_wizard::SubmitOutcome::InvalidCode => setup_token_error_frame(
                crate::setup_token_wizard::SetupTokenErrorCode::InvalidCode,
                "授權碼不正確或已失效，請重新開始並再試一次。",
            ),
            crate::setup_token_wizard::SubmitOutcome::Timeout => setup_token_error_frame(
                crate::setup_token_wizard::SetupTokenErrorCode::Timeout,
                "等待逾時，請重新開始。",
            ),
        }
    }
}
